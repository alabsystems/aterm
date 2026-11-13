// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::test_support::{SessionCtx, Store, new_store, test_handle};
use aterm_types::fabric_grammar::{PRINCIPAL_MAX, PRINCIPAL_NAME_MAX, VIA_MAX_HOPS};

use super::with_link_reset as with_link;

fn registered(store: &Store) -> (String, std::sync::Arc<SessionCtx>) {
    registered_as(store, 1)
}

fn attach_up(store: &Store, generation: BridgeGeneration) {
    bridge_attached(generation);
    assert!(link_report(store, Some(generation), true, "", Some(1)));
}

/// A minute in ms, the `--wait=` of a post whose landing the test delivers itself.
fn hang_ms() -> u64 {
    u64::try_from(aterm_spec::wait::MUST_HAPPEN.as_millis()).expect("a minute in milliseconds")
}

/// For a section that settles every debt it arms: a debt goes stale at twice a hang
/// detector, not the instance's 6 s, so half the horizon is still one.
fn stale_after_a_hang() {
    STALE_AFTER_IN_TEST.store(2 * hang_ms(), Ordering::Relaxed);
}

/// On a reader thread whose fetch the test answers itself: the read waits a hang detector,
/// not the product's 10 s.
fn fetch_waits_a_hang() {
    TEST_FETCH_WAIT.with(|wait| wait.set(Some(aterm_spec::wait::MUST_HAPPEN)));
}

fn on_lane<T>(generation: BridgeGeneration, f: impl FnOnce() -> T) -> T {
    struct Restore(Option<BridgeGeneration>);
    impl Drop for Restore {
        fn drop(&mut self) {
            LANE_GENERATION.with(|lane| lane.set(self.0));
        }
    }
    let _restore = Restore(lane_generation());
    set_lane_generation(generation);
    f()
}

pub(super) fn registered_as(store: &Store, local_id: u64) -> (String, std::sync::Arc<SessionCtx>) {
    let h = test_handle(local_id);
    let sid = h.sid.as_str().to_string();
    let ctx = h.ctx.clone();
    store.write().unwrap_or_else(|p| p.into_inner()).register(h);
    (sid, ctx)
}

pub(super) fn deliver(
    store: &Store,
    sid: &str,
    off: u64,
    from: &str,
    kind: &str,
    text: &str,
) -> String {
    cmd_deliver(
        store,
        &format!("{sid} off={off} from={from} kind={kind} trust=agent text={text}"),
    )
}

pub(super) fn broadcast(store: &Store, sid: &str, off: u64, topic: &str, text: &str) -> String {
    cmd_deliver(
        store,
        &format!(
            "{sid} off={off} from=s-peer@n-lab kind=note trust=agent topic={topic} text={text}"
        ),
    )
}

#[test]
fn mail_since_answers_metadata_a_cursor_and_the_oldest_row_held() {
    let store = new_store();
    let (sid, ctx) = registered(&store);
    for (n, (from, kind)) in [
        ("h-andrew", "task"),
        ("s-peer", "note"),
        ("h-andrew", "ask"),
    ]
    .iter()
    .enumerate()
    {
        let off = 100 + n as u64;
        assert!(deliver(&store, &sid, off, from, kind, "the%20words").starts_with("OK"));
    }

    let all = ctx.fabric.mail_since(0, &MailFilter::default());
    assert_eq!(all.rows.len(), 3, "{:?}", all.rows);
    assert_eq!(all.high, 3);
    assert_eq!(all.present, 3, "the ring holds all three");
    assert_eq!(
        all.rows[0],
        MailLine {
            id: 1,
            off: 100,
            from: "h-andrew".to_string(),
            kind: "task".to_string(),
            re: None,
            topic: None,
        }
    );

    let after = ctx.fabric.mail_since(2, &MailFilter::default());
    assert_eq!(after.rows.iter().map(|r| r.id).collect::<Vec<_>>(), [3]);

    let kinds = MailFilter {
        kinds: vec!["task".to_string(), "ask".to_string()],
        from: None,
        topic: None,
    };
    let got = ctx.fabric.mail_since(0, &kinds);
    assert_eq!(got.rows.iter().map(|r| r.id).collect::<Vec<_>>(), [1, 3]);
    assert_eq!(got.high, 3, "a filtered-out row still moves the cursor");
    let human = MailFilter {
        kinds: Vec::new(),
        from: Some("human".to_string()),
        topic: None,
    };
    assert_eq!(
        ctx.fabric
            .mail_since(0, &human)
            .rows
            .iter()
            .map(|r| r.id)
            .collect::<Vec<_>>(),
        [1, 3]
    );
    let exact = MailFilter {
        kinds: Vec::new(),
        from: Some("s-peer".to_string()),
        topic: None,
    };
    assert_eq!(
        ctx.fabric
            .mail_since(0, &exact)
            .rows
            .iter()
            .map(|r| r.id)
            .collect::<Vec<_>>(),
        [2]
    );
    assert_eq!(sender_class("h-a"), "human");
    assert_eq!(sender_class("s-a"), "agent");
    assert_eq!(sender_class("a-a"), "service");
    assert_eq!(sender_class("weird"), "other");
}

#[test]
fn mail_since_reports_what_was_evicted_even_when_the_front_row_is_pinned() {
    let store = new_store();
    let (sid, ctx) = registered(&store);
    assert!(deliver(&store, &sid, 100, "h-andrew", "task", "x").starts_with("OK"));
    let overflow = RING_CAP + 100;
    for n in 1..overflow {
        let off = 1000 + n as u64;
        let from = format!("s-p{n}");
        let reply = deliver(&store, &sid, off, &from, "note", "x");
        assert!(reply.starts_with("OK"), "row {n}: {reply}");
    }
    let since = ctx.fabric.mail_since(0, &MailFilter::default());
    assert_eq!(since.high, overflow as u64, "every row was issued");
    assert_eq!(
        since.present, RING_CAP as u64,
        "the ring holds its cap, no more"
    );
    let lost = since.high - since.present;
    assert_eq!(lost, (overflow - RING_CAP) as u64);
    assert!(lost > 0, "this fixture must actually lose rows");
    assert_eq!(
        ctx.fabric.lock().rows.front().map(|r| r.id),
        Some(1),
        "the human row is pinned at the front, which is the trap"
    );
}

fn bridge_hold(store: &Store, rest: &str) -> String {
    cmd_hold(store, rest, HoldIssuer::Bridge)
}

fn owner_hold(store: &Store, rest: &str) -> String {
    cmd_hold(store, rest, HoldIssuer::Owner)
}

#[test]
fn a_coalesced_hold_release_is_visible_without_an_intermediate_observation() {
    let store = new_store();
    let (sid, ctx) = registered(&store);
    let before = ctx.fabric.hold_observation();
    assert!(!before.is_held());
    assert_eq!(before.released_at(), None);

    assert_eq!(
        owner_hold(&store, &format!("{sid} on reason=wait")),
        "OK hold=1\n"
    );
    assert_eq!(owner_hold(&store, &format!("{sid} off")), "OK hold=0\n");

    let after = ctx.fabric.hold_observation();
    assert!(!after.is_held());
    assert!(after.released_at().is_some());
    assert!(!after.agrees_unheld(before));
    assert!(after.agrees_unheld(after));
    assert_eq!(
        ctx.fabric
            .run_unfenced(std::time::Duration::ZERO, |observed| observed),
        Some(after),
        "the task transaction receives the same locked hold history"
    );
}

#[test]
fn slot_edits_direct_resets_and_fences_do_not_create_aggregate_release_time() {
    let store = new_store();
    let (sid, ctx) = registered(&store);
    assert_eq!(
        owner_hold(&store, &format!("{sid} on reason=first")),
        "OK hold=1\n"
    );
    let held = ctx.fabric.hold_observation();
    let serial = ctx.fabric.hold_serial();

    assert!(
        ctx.fabric
            .note_direct_reset(Some(std::time::Duration::ZERO))
    );
    assert_ne!(
        ctx.fabric.hold_serial(),
        serial,
        "the existing handoff serial still moves"
    );
    assert_eq!(ctx.fabric.hold_observation(), held);
    assert_eq!(
        owner_hold(&store, &format!("{sid} on reason=second")),
        "OK hold=1\n"
    );
    assert_eq!(
        owner_hold(&store, &format!("{sid} on reason=second")),
        "OK hold=1\n"
    );
    assert_eq!(ctx.fabric.hold_observation(), held);

    assert_eq!(
        apply_hold(
            &ctx,
            HoldAct::Set(Hold {
                origin: "fleet".into(),
                reason: "bridge".into()
            }),
            HoldIssuer::Bridge,
            Some(std::time::Duration::ZERO),
            &|| {},
        ),
        Applied::Changed { held: true }
    );
    assert_eq!(ctx.fabric.hold_observation(), held);
    assert_eq!(owner_hold(&store, &format!("{sid} off")), "OK hold=1\n");
    assert_eq!(
        ctx.fabric.hold_observation(),
        held,
        "the fleet slot still holds it"
    );

    ctx.fabric.fence_hold(true);
    assert_eq!(ctx.fabric.hold_observation(), held);
    assert_eq!(
        ctx.fabric
            .run_unfenced(std::time::Duration::ZERO, |_| panic!(
                "the fence must refuse"
            )),
        None::<()>
    );
    ctx.fabric.lift_hold_fence();
    assert_eq!(ctx.fabric.hold_observation(), held);
    assert_eq!(
        apply_hold(
            &ctx,
            HoldAct::Lift("fleet"),
            HoldIssuer::Bridge,
            Some(std::time::Duration::ZERO),
            &|| {},
        ),
        Applied::Changed { held: false }
    );
    let released = ctx.fabric.hold_observation();
    assert!(released.released_at().is_some());
    assert!(!released.is_held());
    assert!(!released.agrees_unheld(held));
}

#[test]
fn seeding_holds_preserves_the_release_clock_until_the_whole_hold_clears() {
    let store = new_store();
    let (sid, ctx) = registered(&store);
    let initial = ctx.fabric.hold_observation();
    ctx.fabric.seed_holds(Holds::default(), &ctx.timeline);
    assert_eq!(ctx.fabric.hold_observation(), initial);

    let local = parse_holds(["local carried"]);
    ctx.fabric.seed_holds(local.clone(), &ctx.timeline);
    let held = ctx.fabric.hold_observation();
    assert!(held.is_held());
    assert_eq!(held.released_at(), None);
    ctx.fabric.seed_holds(local, &ctx.timeline);
    ctx.fabric
        .seed_holds(parse_holds(["fleet another"]), &ctx.timeline);
    assert_eq!(ctx.fabric.hold_observation(), held);
    assert_eq!(owner_hold(&store, &format!("{sid} off")), "OK hold=1\n");
    assert_eq!(ctx.fabric.hold_observation(), held);
    assert!(apply_hold_for_test(&ctx, None));

    let released = ctx.fabric.hold_observation();
    let release_time = released
        .released_at()
        .expect("the aggregate really cleared");
    assert!(released.agrees_unheld(released));
    ctx.fabric
        .seed_holds(parse_holds(["local later"]), &ctx.timeline);
    let held_again = ctx.fabric.hold_observation();
    assert!(held_again.is_held());
    assert_eq!(held_again.released_at(), Some(release_time));
    assert!(!held_again.agrees_unheld(released));
}

#[test]
fn a_person_replacing_a_remote_hold_does_not_invent_a_release() {
    let store = new_store();
    let (sid, ctx) = registered(&store);
    let driver = Driver {
        node: "03".repeat(16),
        host: "mac-03".into(),
    };
    {
        let _driving = DrivingAs::enter(driver);
        assert_eq!(
            cmd_hold(
                &store,
                &format!("{sid} on reason=drive"),
                HoldIssuer::Remote
            ),
            "OK hold=1\n"
        );
    }
    let remote = ctx.fabric.hold_observation();
    assert!(remote.is_held());
    assert_eq!(person_hold(&ctx, true, &|| {}), PersonHoldOutcome::Moved);
    assert!(
        ctx.fabric.holds().remote().is_none(),
        "the person actually replaced the remote slot"
    );
    assert!(ctx.fabric.holds().person().is_some());
    assert_eq!(ctx.fabric.hold_observation(), remote);

    assert_eq!(person_hold(&ctx, false, &|| {}), PersonHoldOutcome::Moved);
    let released = ctx.fabric.hold_observation();
    assert!(!released.is_held());
    assert!(released.released_at().is_some());
    assert!(!released.agrees_unheld(remote));
}

#[cfg(unix)]
#[test]
fn a_committing_update_holds_every_hold_back_and_drops_none() {
    let store = new_store();
    let (sid, ctx) = registered(&store);
    let serial = ctx.fabric.fence_hold(true);
    assert_eq!(
        cmd_hold_within(
            &store,
            &format!("{sid} on reason=stop"),
            HoldIssuer::Owner,
            Some(std::time::Duration::from_millis(50)),
        ),
        HOLD_BUSY
    );
    assert_eq!(ctx.fabric.hold(), None);
    assert_eq!(ctx.fabric.hold_serial(), serial, "nothing moved");

    let before = ctx.fabric.fence_waits();
    let waiter = {
        let ctx = std::sync::Arc::clone(&ctx);
        std::thread::spawn(move || {
            apply_hold(
                &ctx,
                HoldAct::Set(Hold {
                    reason: "fabric-lost".to_string(),
                    origin: "fleet".to_string(),
                }),
                HoldIssuer::Bridge,
                None,
                &|| {},
            )
        })
    };
    assert_parked_on_the_fence(
        &ctx.fabric,
        before,
        || waiter.is_finished(),
        "the sweep waits out the Commit",
    );
    assert_eq!(ctx.fabric.hold(), None);
    assert_eq!(ctx.fabric.hold_serial(), serial);
    ctx.fabric.lift_hold_fence();
    assert_eq!(
        waiter.join().expect("the sweep"),
        Applied::Changed { held: true }
    );
    assert_eq!(
        ctx.fabric.hold().map(|hold| hold.origin),
        Some("fleet".to_string())
    );
    assert_ne!(ctx.fabric.hold_serial(), serial);
    assert_eq!(owner_hold(&store, &format!("{sid} off")), HOLD_DENIED);
}

#[test]
fn post_and_deliver_hold_their_words_to_the_fabric_grammar() {
    let name = |n: usize| "a".repeat(n);
    let store = new_store();
    let (sid, ctx) = registered(&store);

    let long = format!("h-{}", name(PRINCIPAL_NAME_MAX + 1));
    assert_eq!(
        cmd_post(&ctx, &format!("to={long} kind=note hi"), None),
        POST_USAGE
    );
    assert_eq!(
        cmd_deliver(
            &store,
            &format!("{sid} off=1 from={long} kind=note trust=human text=x")
        ),
        DELIVER_USAGE
    );
    assert!(valid_principal(&format!(
        "s-{}@n-{}",
        name(PRINCIPAL_NAME_MAX),
        name(PRINCIPAL_NAME_MAX)
    )));
    assert!(!valid_principal(&format!(
        "s-{}@n-{}",
        name(PRINCIPAL_NAME_MAX),
        name(PRINCIPAL_NAME_MAX + 1)
    )));

    let hop = format!("s-{}", name(PRINCIPAL_NAME_MAX));
    let over = vec![hop.as_str(); VIA_MAX_HOPS + 1].join(",");
    assert_eq!(
        cmd_post(&ctx, &format!("to=@s-b kind=note via={over} x"), None),
        POST_USAGE
    );
    assert_eq!(
        cmd_post(&ctx, "to=@s-b kind=note via=s-a@n-b x", None),
        POST_USAGE
    );
    assert_eq!(
        cmd_deliver(
            &store,
            &format!("{sid} off=1 from=h-a kind=note trust=human via={over} text=x")
        ),
        DELIVER_USAGE
    );
    assert_eq!(
        cmd_post(&ctx, "to=@s-b kind=note via=s-a x", None),
        "OK 1\n"
    );
}

#[test]
fn a_queued_post_counts_its_via_and_a_retired_one_keeps_neither() {
    let store = new_store();
    let (sid, ctx) = registered(&store);
    let via = vec!["s-aaaaaaaa"; VIA_MAX_HOPS].join(",");
    let body = "x".repeat(1000);
    assert_eq!(
        cmd_post(&ctx, &format!("to=@s-b kind=note via={via} {body}"), None),
        "OK 1\n"
    );
    let (queued, bytes) = ctx.fabric.lock().queued_load();
    assert_eq!(queued, 1);
    assert_eq!(
        bytes,
        body.len() + via.len(),
        "the byte bound counts the body AND the relay chain"
    );

    assert_eq!(
        cmd_outbox_sent(&store, &format!("{sid} 1 off=91")),
        "OK\n",
        "the bridge published it"
    );
    let inbox = ctx.fabric.lock();
    let row = inbox.posts.iter().find(|p| p.id == 1).expect("the row");
    assert_eq!(row.off, Some(91));
    assert!(row.body.is_empty(), "the body goes at retirement");
    assert_eq!(
        row.via, None,
        "and so does the relay chain: a retired row is kept only to resolve \
         an old answer's `re-id=`, and 512 of them are held for the life of \
         the session"
    );
    assert_eq!(row.caller_sized_bytes(), 0);
    drop(inbox);
    assert_eq!(ctx.fabric.lock().queued_load(), (0, 0));
}

#[test]
fn an_address_carries_one_at_sign_and_not_a_run_of_them() {
    let store = new_store();
    let (_sid, ctx) = registered(&store);
    assert_eq!(cmd_post(&ctx, "to=@s-abc kind=note hi", None), "OK 1\n");
    assert_eq!(
        cmd_post(&ctx, "to=@s-abc@n-lab kind=note hi", None),
        "OK 2\n"
    );
    assert_eq!(cmd_post(&ctx, "to=h-andrew kind=note hi", None), "OK 3\n");
    assert_eq!(cmd_post(&ctx, "to=say kind=note hi", None), "OK 4\n");
    assert_eq!(cmd_post(&ctx, "to=@@s-abc kind=note hi", None), POST_USAGE);
    let padded = format!("{}s-abc", "@".repeat(4096));
    assert_eq!(
        cmd_post(&ctx, &format!("to={padded} kind=note hi"), None),
        POST_USAGE
    );
    let inbox = ctx.fabric.lock();
    assert_eq!(inbox.posts.len(), 4, "the two padded ones never queued");
    for row in &inbox.posts {
        assert!(
            row.to.len() <= 2 * PRINCIPAL_MAX + 2,
            "`to` is bounded by the principal grammar: {}",
            row.to.len()
        );
    }
}

/// `re=` takes a bus offset bare or as `inbox`, `cause=` and a brief's `head=` print it
/// (`@<off>`), as `aterm fabric post` does: both forms post the same reply, and `await inbox
/// re=` reads both.
#[test]
fn a_reply_names_its_offset_with_or_without_the_at_sign() {
    let store = new_store();
    let (_sid, ctx) = registered(&store);
    assert_eq!(
        cmd_post(&ctx, "to=@s-abc kind=note re=21 hi", None),
        "OK 1\n"
    );
    assert_eq!(
        cmd_post(&ctx, "to=@s-abc kind=note re=@21 hi", None),
        "OK 2\n"
    );
    for bad in ["re=@", "re=@@21", "re=+5", "re=x"] {
        assert_eq!(
            cmd_post(&ctx, &format!("to=@s-abc kind=note {bad} hi"), None),
            POST_USAGE,
            "{bad}"
        );
    }
    let res: Vec<Option<u64>> = ctx.fabric.lock().posts.iter().map(|r| r.re).collect();
    assert_eq!(res, [Some(21), Some(21)]);
    let (bare, at) = (
        cmd_await_inbox(&ctx, &["re=7"], 0),
        cmd_await_inbox(&ctx, &["re=@7"], 0),
    );
    assert!(!at.starts_with("ERR usage"), "{at}");
    assert_eq!(at, bare);
}

#[test]
fn the_door_measures_the_row_the_fold_will_measure() {
    let store = new_store();
    let (_sid, ctx) = registered(&store);
    let fills = OUTBOX_BYTES_MAX / BODY_MAX - 1;
    for i in 0..fills {
        assert_eq!(
            cmd_post(
                &ctx,
                &format!("to=@s-abc kind=note len={BODY_MAX}"),
                Some(vec![b'x'; BODY_MAX])
            ),
            format!("OK {}\n", i + 1)
        );
    }
    assert_eq!(
        ctx.fabric.lock().queued_load(),
        (fills, OUTBOX_BYTES_MAX - BODY_MAX)
    );

    let via = format!("s-{}", "a".repeat(18));
    assert_eq!(via.len(), 20);
    assert!(
        cmd_post(
            &ctx,
            &format!("to=@s-abc kind=note via={via} len={}", BODY_MAX - 10),
            Some(vec![b'x'; BODY_MAX - 10])
        )
        .starts_with("ERR outbox full"),
        "the incoming row's relay chain counts too"
    );

    assert!(
        cmd_post(
            &ctx,
            &format!("to=@s-abc kind=note len={}", 100 * 1024),
            Some(vec![0xff; 100 * 1024])
        )
        .starts_with("ERR outbox full"),
        "a frame of non-UTF-8 is weighed as the row it becomes"
    );

    assert_eq!(
        cmd_post(
            &ctx,
            &format!("to=@s-abc kind=note len={}", BODY_MAX - 10),
            Some(vec![b'x'; BODY_MAX - 10])
        ),
        format!("OK {}\n", fills + 1)
    );
    assert_eq!(
        ctx.fabric.lock().queued_load(),
        (fills + 1, OUTBOX_BYTES_MAX - 10)
    );
}

fn rows(reply: &str) -> Vec<&str> {
    reply.lines().skip(1).collect()
}

#[test]
fn the_header_count_is_every_row_that_follows() {
    let store = new_store();
    let (sid, ctx) = registered(&store);
    assert_eq!(
        deliver(&store, &sid, 10, "h-andrew", "task", "go"),
        "OK 1\n"
    );
    assert_eq!(cmd_post(&ctx, "to=@s-abc kind=note hello", None), "OK 1\n");
    let reply = cmd_inbox(&ctx, "");
    let header = reply.lines().next().expect("a header");
    assert!(header.starts_with("OK 2 "), "{header}");
    assert_eq!(rows(&reply).len(), 2, "{reply}");
    assert!(rows(&reply)[0].starts_with("msg 1 off=10 "), "{reply}");
    assert!(
        rows(&reply)[1].starts_with("post 1 to=@s-abc kind=note off=-"),
        "{reply}"
    );
    for field in [
        "hold=0",
        "holder=-",
        "seen=0",
        "bus_head=10",
        "dropped=0",
        "pending=0",
    ] {
        assert!(header.contains(field), "header lacks {field}: {header}");
    }
}

#[test]
fn deliver_is_idempotent_on_the_offset() {
    let store = new_store();
    let (sid, ctx) = registered(&store);
    assert_eq!(deliver(&store, &sid, 90312, "h-a", "ask", "one"), "OK 1\n");
    assert_eq!(
        deliver(&store, &sid, 90312, "h-a", "ask", "one"),
        "OK 1\n",
        "a redelivered offset answers the id it first got"
    );
    assert_eq!(
        deliver(&store, &sid, 90312, "h-a", "ask", "two"),
        "OK 1\n",
        "the offset decides, not the payload"
    );
    assert_eq!(rows(&cmd_inbox(&ctx, "--peek")).len(), 1);
    assert_eq!(deliver(&store, &sid, 90313, "h-a", "ask", "next"), "OK 2\n");
    assert_eq!(rows(&cmd_inbox(&ctx, "--peek")).len(), 2);
}

/// A RECORD MOVED ACROSS THIS MAC'S KEY ROTATION IS SHOWN ONCE (§11 C3b): the old node's
/// bridge delivered `in/<old>` mail at offset 7 and went before its commit, so the move put a
/// copy on this node's lane at 40, naming 7 (`was=7`). The endpoint answers the copy with the
/// row 7 got, showing nothing, and a second copy (a later rotation moved it again, at 60) is
/// answered too, whether its bridge names 7 or, where it could not read back past the first
/// copy, 40: an answered copy is remembered by its own offset as well. A moved record whose
/// first offset it never showed is shown, once.
/// NEGATIVE CONTROL: the same copy with no `was=`, as a bridge before lane 11 sends it, is a
/// second row.
#[test]
fn a_moved_copy_of_mail_shown_before_the_move_is_not_shown_again() {
    let store = new_store();
    let (sid, ctx) = registered(&store);
    let moved = |off: u64, was: u64| {
        cmd_deliver(
            &store,
            &format!("{sid} off={off} from=s-b@n-peer kind=ask trust=agent was={was} text=read"),
        )
    };
    assert_eq!(
        deliver(&store, &sid, 7, "s-b@n-peer", "ask", "read"),
        "OK 1\n"
    );
    assert_eq!(moved(40, 7), "OK 1\n", "answered as the row 7 got");
    assert_eq!(moved(60, 7), "OK 1\n", "and again for a copy moved twice");
    assert_eq!(
        moved(70, 40),
        "OK 0\n",
        "a copy that names an answered copy is answered from its memory"
    );
    assert_eq!(moved(80, 9), "OK 2\n", "mail the move took unread is shown");
    assert_eq!(
        moved(90, 9),
        "OK 0\n",
        "once: answered from the memory of 9, as a repeat held by offset alone is"
    );
    assert_eq!(
        ctx.fabric.lock().rows.len(),
        2,
        "{:?}",
        ctx.fabric.lock().rows
    );

    assert_eq!(
        deliver(&store, &sid, 41, "s-b@n-peer", "ask", "read"),
        "OK 3\n",
        "without was= the copy is another row"
    );
    assert_eq!(
        cmd_deliver(
            &store,
            &format!("{sid} off=42 from=s-b@n-peer kind=ask was=x")
        ),
        DELIVER_USAGE,
        "was= names an offset"
    );
}

#[test]
fn a_walk_again_neither_duplicates_a_held_row_nor_counts_a_refusal_twice() {
    let store = new_store();
    let (sid, ctx) = registered(&store);
    assert_eq!(
        ctx.fabric.topic_add("news", "head", &ctx.timeline),
        Ok(true)
    );
    assert_eq!(deliver(&store, &sid, 1, "h-a", "task", "keep"), "OK 1\n");
    let cap = u64::try_from(DEDUP_CAP).unwrap();
    for i in 0..cap {
        let from = format!("s-x@n-p{i}");
        assert!(deliver(&store, &sid, 100 + i, &from, "note", "n").starts_with("OK "));
    }
    assert!(
        !ctx.fabric.lock().dedup_set.contains(&1),
        "the window forgot it"
    );
    assert_eq!(
        deliver(&store, &sid, 1, "h-a", "task", "keep"),
        "OK 1\n",
        "the ring still holds it: the id it got, no second row"
    );
    assert_eq!(
        ctx.fabric.lock().rows.iter().filter(|r| r.off == 1).count(),
        1
    );

    let fresh = new_store();
    let (bsid, bctx) = registered(&fresh);
    assert_eq!(
        bctx.fabric.topic_add("news", "head", &bctx.timeline),
        Ok(true)
    );
    let quota = u64::try_from(SENDER_QUOTA).unwrap();
    for off in 0..quota {
        assert!(broadcast(&fresh, &bsid, 5000 + off, "news", "b").starts_with("OK "));
    }
    let refused = 5000 + quota;
    assert_eq!(
        broadcast(&fresh, &bsid, refused, "news", "b"),
        "ERR quota\n"
    );
    assert_eq!(bctx.fabric.lock().dropped, 1);
    assert_eq!(
        broadcast(&fresh, &bsid, refused, "news", "b"),
        "OK 0\n",
        "a walk again: taken once, held no more"
    );
    assert_eq!(bctx.fabric.lock().dropped, 1, "counted once");
    assert_eq!(broadcast(&fresh, &bsid, 5000, "news", "b"), "OK 1\n");
    assert_eq!(
        broadcast(&fresh, &bsid, refused + 1, "news", "b"),
        "ERR quota\n",
        "an offset neither holds is judged as new"
    );
    for off in 0..quota {
        let reply = deliver(&fresh, &bsid, 9000 + off, "s-peer@n-far", "note", "a");
        assert!(reply.starts_with("OK "), "{reply}");
    }
    let addressed = 9000 + quota;
    for _ in 0..2 {
        assert_eq!(
            deliver(&fresh, &bsid, addressed, "s-peer@n-far", "note", "a"),
            "ERR quota\n"
        );
    }
    assert_eq!(
        bctx.fabric.lock().dropped,
        2,
        "the second broadcast refusal"
    );
}

#[test]
fn a_carried_mark_takes_its_refill_back_without_a_second_mail_in() {
    let mail_ins = |ctx: &SessionCtx| {
        ctx.timeline
            .lock()
            .unwrap()
            .facts
            .iter()
            .filter(|f| f.starts_with("inbox: MailIn"))
            .count()
    };
    let quota = u64::try_from(SENDER_QUOTA).unwrap();
    let refused = 5000 + quota;
    let carry = with_link(|| {
        let store = new_store();
        let (sid, ctx) = registered(&store);
        assert_eq!(
            ctx.fabric.topic_add("news", "head", &ctx.timeline),
            Ok(true)
        );
        assert_eq!(deliver(&store, &sid, 7, "h-a", "task", "t"), "OK 1\n");
        for off in 5000..refused {
            assert!(broadcast(&store, &sid, off, "news", "b").starts_with("OK "));
        }
        assert_eq!(broadcast(&store, &sid, refused, "news", "b"), "ERR quota\n");
        assert_eq!(mail_ins(&ctx), 1 + usize::try_from(quota).unwrap());
        ctx.fabric.handoff_carry(&mut CARRY_MAIL_BYTES.clone())
    });
    assert_eq!(carry.marked.first(), Some(&7), "oldest first");
    assert_eq!(carry.marked.len(), 1 + usize::try_from(quota).unwrap());
    assert!(
        !carry.marked.contains(&refused),
        "a refusal marked nothing, so its refill is marked as new"
    );
    let wire = aterm_toml::to_string(&carry).expect("serializes");
    let carry: FabricCarry = aterm_toml::from_str(&wire).expect("reads back");
    assert!(wire.len() <= carry.rows_cost() + 512, "{wire}");

    for log_carried in [true, false] {
        with_link(|| {
            let store = new_store();
            let (sid, ctx) = registered(&store);
            ctx.timeline.lock().unwrap().log_carried = log_carried;
            ctx.fabric.seed_carry(&carry, &ctx.timeline);
            assert_eq!(
                ctx.fabric.topic_add("news", "head", &ctx.timeline),
                Ok(true)
            );
            let before = mail_ins(&ctx);
            let id = carry.last_msg_id + 1;
            assert_eq!(
                deliver(&store, &sid, 7, "h-a", "task", "t"),
                format!("OK {id}\n"),
                "the refill is taken back into the ring (log carried: {log_carried})"
            );
            assert!(
                ctx.fabric.lock().row_at(7).is_some(),
                "the row is held again"
            );
            assert_eq!(
                deliver(&store, &sid, 7, "h-a", "task", "t"),
                format!("OK {id}\n"),
                "and once only"
            );
            assert_eq!(
                mail_ins(&ctx) - before,
                usize::from(!log_carried),
                "a carried log already marks it; a log that started over does not"
            );
            assert!(broadcast(&store, &sid, refused, "news", "b").starts_with("OK "));
            assert_eq!(
                deliver(&store, &sid, 8, "h-a", "task", "n"),
                format!("OK {}\n", id + 2)
            );
            assert_eq!(
                mail_ins(&ctx) - before,
                2 + usize::from(!log_carried),
                "the refused offset and a new one are marked"
            );
        });
    }

    with_link(|| {
        let store = new_store();
        let (sid, ctx) = registered(&store);
        ctx.timeline.lock().unwrap().log_carried = true;
        ctx.fabric.seed_carry(&carry, &ctx.timeline);
        assert!(
            ctx.fabric.lock().holds_bus_references(),
            "carried marks are offsets on the carried log"
        );
        let generation = next_bridge_generation();
        attach_up(&store, generation);
        assert_eq!(
            cmd_link(
                &store,
                &format!("log {}.0", "b".repeat(32)),
                Some(generation)
            ),
            "OK forgot=1\n"
        );
        let before = mail_ins(&ctx);
        assert!(deliver(&store, &sid, 7, "h-a", "task", "t").starts_with("OK "));
        assert_eq!(
            mail_ins(&ctx) - before,
            1,
            "off=7 on another log is another mail"
        );
    });
}

#[test]
fn a_broadcast_row_is_marked_refilled_only_under_a_bridge_that_said_so() {
    with_link(|| {
        let store = new_store();
        let (sid, ctx) = registered(&store);
        assert_eq!(
            ctx.fabric.topic_add("news", "head", &ctx.timeline),
            Ok(true)
        );
        let generation = next_bridge_generation();
        attach_up(&store, generation);
        let refill_of = |off: u64| {
            ctx.fabric
                .lock()
                .rows
                .iter()
                .find(|r| r.off == off)
                .map(|r| r.refill)
        };
        assert!(broadcast(&store, &sid, 10, "news", "b").starts_with("OK "));
        assert!(cmd_hello("lane=2 build=x", Some(generation)).starts_with("OK "));
        assert!(broadcast(&store, &sid, 11, "news", "b").starts_with("OK "));
        assert!(cmd_hello("lane=2 refill=in", Some(generation)).starts_with("OK "));
        assert!(broadcast(&store, &sid, 12, "news", "b").starts_with("OK "));
        assert!(cmd_hello("lane=2 refill=in,say,later", Some(generation)).starts_with("OK "));
        assert!(broadcast(&store, &sid, 13, "news", "b").starts_with("OK "));
        assert!(deliver(&store, &sid, 14, "h-a", "note", "a").starts_with("OK "));
        assert_eq!(
            [10, 11, 12, 13, 14].map(refill_of),
            [
                Some(false),
                Some(false),
                Some(false),
                Some(true),
                Some(false)
            ]
        );
        assert!(bridge_refills_broadcasts());
        let stale = next_bridge_generation();
        assert!(cmd_hello("lane=2", Some(stale)).starts_with("OK stale=1"));
        assert!(
            bridge_refills_broadcasts(),
            "a ghost lane's hello is recorded nowhere"
        );
    });
}

#[test]
fn the_mirror_is_acknowledged_only_to_a_recorded_hello() {
    with_link(|| {
        let store = new_store();
        let generation = next_bridge_generation();
        attach_up(&store, generation);
        assert_eq!(fabric_mirror_facts(), "", "off: the line as it was");
        let off = cmd_hello("lane=2 build=x", Some(generation));
        assert!(off.starts_with("OK ") && !off.contains("log="), "{off}");
        assert!(!bridge_mirrors_logs());
        let on = cmd_hello("lane=2 build=x log=turns", Some(generation));
        assert!(on.trim_end().ends_with(" log=turns"), "{on}");
        assert!(bridge_mirrors_logs());
        assert_eq!(fabric_mirror_facts(), " bridge_log=turns mirror=starting");
        assert_eq!(cmd_link(&store, "mirror budget", Some(generation)), "OK\n");
        assert_eq!(fabric_mirror_facts(), " bridge_log=turns mirror=budget");
        assert_eq!(
            cmd_link(&store, "mirror stopped reason=refused", Some(generation)),
            "OK\n"
        );
        assert_eq!(
            fabric_mirror_facts(),
            " bridge_log=turns mirror=stopped:refused"
        );
        let ghost = next_bridge_generation();
        assert!(lane_owns(Some(generation)) && !lane_owns(Some(ghost)));
        let stale = cmd_hello("lane=2 log=turns", Some(ghost));
        assert!(
            stale.starts_with("OK stale=1") && !stale.contains("log="),
            "a ghost lane is never told the mirror is served: {stale}"
        );
        assert_eq!(cmd_link(&store, "mirror on", Some(ghost)), "OK stale=1\n");
        assert_eq!(
            fabric_mirror_facts(),
            " bridge_log=turns mirror=stopped:refused",
            "a ghost lane's report records nothing"
        );
        assert!(cmd_link(&store, "mirror sideways", Some(generation)).starts_with("ERR usage"));
        let next = next_bridge_generation();
        attach_up(&store, next);
        assert!(!bridge_mirrors_logs());
        assert_eq!(fabric_mirror_facts(), "");
    });
}

#[test]
fn an_auto_hello_asks_for_nothing_and_says_unretained() {
    with_link(|| {
        let store = new_store();
        let generation = next_bridge_generation();
        attach_up(&store, generation);
        let auto = cmd_hello("lane=5 build=x log=auto", Some(generation));
        assert!(auto.starts_with("OK ") && !auto.contains("log="), "{auto}");
        assert!(!bridge_mirrors_logs(), "auto asks for nothing yet");
        assert_eq!(fabric_mirror_facts(), " bridge_log=auto mirror=starting");
        assert_eq!(
            cmd_link(&store, "mirror unretained", Some(generation)),
            "OK\n"
        );
        assert_eq!(fabric_mirror_facts(), " bridge_log=auto mirror=unretained");
        assert!(!bridge_mirrors_logs());
        let turns = cmd_hello("lane=5 build=x log=turns", Some(generation));
        assert!(turns.trim_end().ends_with(" log=turns"), "{turns}");
        assert!(bridge_mirrors_logs());
        assert_eq!(cmd_link(&store, "mirror on", Some(generation)), "OK\n");
        assert_eq!(fabric_mirror_facts(), " bridge_log=turns mirror=on");
    });
}

/// `outbox`'s `roster=` is read after its listing: a session that left during the walk has
/// moved the count it names, so a bridge looks before it routes a post queued after that
/// exit. Read with the walk's snapshot it would name 0 here, the count before the walk.
#[test]
fn outbox_names_the_roster_count_as_it_stands_after_the_listing() {
    use std::sync::atomic::{AtomicU64, Ordering};

    struct Walked {
        inner: Arc<SessionCtx>,
        seq: Arc<AtomicU64>,
    }
    impl FabricSession for Walked {
        fn self_id(&self) -> &SessionId {
            self.inner.self_id()
        }
        fn fabric(&self) -> &SessionFabric {
            self.seq.fetch_add(1, Ordering::SeqCst);
            self.inner.fabric()
        }
        fn record_fact(&self, kind: &'static str, fact: FabricFact<'_>) -> bool {
            self.inner.record_fact(kind, fact)
        }
        fn fabric_changed(&self) {}
        fn holder_token(&self) -> Option<String> {
            None
        }
    }
    struct OneWalked(Arc<dyn FabricSession>, Arc<AtomicU64>);
    impl Registry for OneWalked {
        fn sessions(&self) -> Vec<Arc<dyn FabricSession>> {
            vec![Arc::clone(&self.0)]
        }
        fn session(&self, _: &SessionId) -> Option<Arc<dyn FabricSession>> {
            Some(Arc::clone(&self.0))
        }
        fn roster_seq(&self) -> u64 {
            self.1.load(Ordering::SeqCst)
        }
        fn bridge_lost_advised(&self, _: usize) {}
        fn torn_state_set_aside(&self, _: u64, _: bool) {}
        fn torn_state_held(&self, _: u64) {}
    }

    let seq = Arc::new(AtomicU64::new(0));
    let walked = Walked {
        inner: test_handle(1).ctx,
        seq: Arc::clone(&seq),
    };
    let store = Arc::new(RwLock::new(OneWalked(Arc::new(walked), Arc::clone(&seq))));
    let reply = cmd_outbox(&store, "");
    let moved = seq.load(Ordering::SeqCst);
    assert!(moved > 0, "the walk reached the session");
    assert_eq!(reply, format!("OK 0 roster={moved}\n"));
}

#[test]
fn a_set_aside_count_reaches_the_registry_only_from_the_owning_lane() {
    struct ReplaceDuringPublication {
        owner: BridgeGeneration,
        next: BridgeGeneration,
        received: Mutex<Vec<(u64, bool)>>,
    }

    impl Registry for ReplaceDuringPublication {
        fn sessions(&self) -> Vec<Arc<dyn FabricSession>> {
            Vec::new()
        }
        fn session(&self, _: &SessionId) -> Option<Arc<dyn FabricSession>> {
            None
        }
        fn roster_seq(&self) -> u64 {
            0
        }
        fn bridge_lost_advised(&self, _: usize) {}
        fn torn_state_set_aside(&self, files: u64, _: bool) {
            let transition = match LINK.log_transition.try_lock() {
                Ok(guard) => Some(guard),
                Err(std::sync::TryLockError::Poisoned(error)) => Some(error.into_inner()),
                Err(std::sync::TryLockError::WouldBlock) => None,
            };
            if let Some(guard) = transition {
                drop(guard);
                bridge_attached(self.next);
            }
            self.received
                .lock()
                .unwrap()
                .push((files, lane_owns(Some(self.owner))));
        }
        fn torn_state_held(&self, _: u64) {}
    }

    with_link(|| {
        let store = new_store();
        let generation = next_bridge_generation();
        attach_up(&store, generation);
        let taken = || store.read().unwrap().set_aside.lock().unwrap().clone();
        assert_eq!(
            cmd_link(&store, "set-aside files=3", Some(generation)),
            "OK\n"
        );
        assert_eq!(taken(), vec![3]);
        let ghost = next_bridge_generation();
        assert_eq!(
            cmd_link(&store, "set-aside files=2", Some(ghost)),
            "OK stale=1\n"
        );
        assert_eq!(cmd_link(&store, "set-aside files=2", None), "OK stale=1\n");
        for bad in [
            "set-aside",
            "set-aside files=0",
            "set-aside files=-1",
            "set-aside files=x",
            "set-aside n=2",
            "set-aside files=2 more",
        ] {
            assert!(
                cmd_link(&store, bad, Some(generation)).starts_with("ERR usage"),
                "{bad}"
            );
        }
        assert_eq!(taken(), vec![3], "nothing but the owner's one line");

        let next = next_bridge_generation();
        let replacing = Arc::new(RwLock::new(ReplaceDuringPublication {
            owner: generation,
            next,
            received: Mutex::new(Vec::new()),
        }));
        assert_eq!(
            cmd_link(&replacing, "set-aside files=4", Some(generation)),
            "OK\n"
        );
        assert_eq!(
            *replacing.read().unwrap().received.lock().unwrap(),
            vec![(4, true)],
            "replacement cannot cross the owning lane's publication"
        );
        attach_up(&store, next);
        assert_eq!(
            cmd_link(&store, "set-aside files=2", Some(generation)),
            "OK stale=1\n"
        );
        assert_eq!(cmd_link(&store, "set-aside files=2", Some(next)), "OK\n");
        assert_eq!(taken(), vec![3, 2]);
    });
}

#[test]
fn a_set_aside_says_whether_its_bridge_tells_when_the_quarantine_is_empty() {
    with_link(|| {
        let store = new_store();
        let generation = next_bridge_generation();
        attach_up(&store, generation);
        let told = || {
            store
                .read()
                .unwrap()
                .told_when_empty
                .lock()
                .unwrap()
                .clone()
        };
        assert_eq!(
            cmd_link(&store, "set-aside files=1", Some(generation)),
            "OK\n"
        );
        assert_eq!(told(), [false], "no hello: lane 1, which never tells");
        assert!(cmd_hello("lane=4 build=x", Some(generation)).starts_with("OK "));
        assert_eq!(
            cmd_link(&store, "set-aside files=1", Some(generation)),
            "OK\n"
        );
        assert_eq!(told(), [false, false], "lane 4 never tells");
        let lane = aterm_types::control_verbs::LINK_QUARANTINE_LANE;
        assert!(cmd_hello(&format!("lane={lane} build=x"), Some(generation)).starts_with("OK "));
        assert_eq!(
            cmd_link(&store, "set-aside files=1", Some(generation)),
            "OK\n"
        );
        assert_eq!(told(), [false, false, true], "lane {lane} tells");
        let next = next_bridge_generation();
        attach_up(&store, next);
        assert_eq!(cmd_link(&store, "set-aside files=1", Some(next)), "OK\n");
        assert_eq!(
            told(),
            [false, false, true, false],
            "the next bridge has said no hello yet"
        );
    });
}

#[test]
fn a_quarantine_count_reaches_the_registry_only_from_the_owning_lane() {
    with_link(|| {
        let store = new_store();
        let generation = next_bridge_generation();
        attach_up(&store, generation);
        let held = || store.read().unwrap().held.lock().unwrap().clone();
        for (line, n) in [("quarantine torn=2", 2), ("quarantine torn=0", 0)] {
            assert_eq!(cmd_link(&store, line, Some(generation)), "OK\n", "{line}");
            assert_eq!(held().last(), Some(&n), "{line}");
        }
        let ghost = next_bridge_generation();
        assert_eq!(
            cmd_link(&store, "quarantine torn=1", Some(ghost)),
            "OK stale=1\n"
        );
        assert_eq!(cmd_link(&store, "quarantine torn=1", None), "OK stale=1\n");
        for bad in [
            "quarantine",
            "quarantine torn=-1",
            "quarantine torn=x",
            "quarantine files=0",
            "quarantine torn=0 more",
        ] {
            assert!(
                cmd_link(&store, bad, Some(generation)).starts_with("ERR usage"),
                "{bad}"
            );
        }
        assert_eq!(held(), vec![2, 0], "nothing but the owner's two lines");
        assert!(
            store.read().unwrap().set_aside.lock().unwrap().is_empty(),
            "a count held is never one set aside"
        );
    });
}

/// A9: `link gap` tells every session its node serves, once per range: the `inbox` header
/// names the range and the broker's count, and the log marks it (`inbox-gap`), each told
/// session's log readers woken; a repeat of the range marks nothing; a log replaced past the
/// range forgets it, and a seamless update carries it, a successor whose log started over
/// marking it there. NC (run, B22): a `note_given_up` that records nothing fails it. NC (run,
/// B22 land): a `seed_carry` that takes the range without marking an uncarried log fails it.
#[test]
fn a_link_gap_tells_every_session_its_given_up_mail_once_in_header_and_log() {
    with_link(|| {
        let store = new_store();
        let (sa, a) = registered_as(&store, 1);
        let (sb, b) = registered_as(&store, 2);
        let generation = next_bridge_generation();
        attach_up(&store, generation);
        let log = format!("{}.0", "c".repeat(32));
        assert_eq!(
            cmd_link(&store, &format!("log {log}"), Some(generation)),
            "OK\n"
        );
        let woke = Mutex::new(Vec::new());
        let tell = |line: &str| {
            cmd_link_waking(&store, line, Some(generation), || {}, &|sid| {
                woke.lock().unwrap().push(sid.as_str().to_string());
            })
        };
        let header = |ctx: &SessionCtx| {
            let out = cmd_inbox(ctx, "--peek --meta");
            out.lines().next().unwrap_or_default().to_string()
        };
        let marks = |ctx: &SessionCtx| -> Vec<String> {
            let tl = ctx.timeline.lock().unwrap();
            tl.facts
                .iter()
                .filter(|f| f.starts_with("inbox-gap: "))
                .cloned()
                .collect()
        };
        assert!(!header(&a).contains("given_up"), "{}", header(&a));

        assert_eq!(tell("gap from=10 to=42 records=3"), "OK\n");
        let first = "inbox-gap: MailGap { from: 10, to: 42, records: Some(3) }";
        for ctx in [&a, &b] {
            assert!(
                header(ctx).ends_with(" pending=0 given_up=@10..@42 given_up_count=3"),
                "{}",
                header(ctx)
            );
            assert_eq!(marks(ctx), [first]);
        }
        assert_eq!(*woke.lock().unwrap(), [sa.clone(), sb.clone()]);

        for repeat in ["gap from=10 to=42", "gap to=42 from=10 records=3"] {
            assert_eq!(tell(repeat), "OK\n", "{repeat}");
        }
        assert_eq!(marks(&a), [first], "a repeat is not marked again");
        assert!(header(&a).ends_with(" given_up_count=3"), "{}", header(&a));
        assert_eq!(woke.lock().unwrap().len(), 2, "nor wakes anyone");

        assert_eq!(tell("gap from=42 to=50"), "OK\n");
        assert!(header(&b).ends_with(" given_up=@42..@50"), "{}", header(&b));
        assert_eq!(marks(&b).len(), 2);

        let ghost = next_bridge_generation();
        assert_eq!(
            cmd_link(&store, "gap from=60 to=70", Some(ghost)),
            "OK stale=1\n"
        );
        assert_eq!(cmd_link(&store, "gap from=60 to=70", None), "OK stale=1\n");
        for bad in [
            "gap",
            "gap from=1",
            "gap to=2",
            "gap from=3 to=2",
            "gap from=1 to=2 records=x",
            "gap from=1 from=1 to=2",
            "gap from=1 to=2 records=1 records=1",
            "gap from=-1 to=2",
            "gap from=1 to=2 more",
        ] {
            assert!(
                cmd_link(&store, bad, Some(generation)).starts_with("ERR usage"),
                "{bad}"
            );
        }
        assert_eq!(
            marks(&a).len(),
            2,
            "a stale or malformed line tells nothing"
        );

        let mut budget = CARRY_MAIL_BYTES;
        let carry = a.fabric.handoff_carry(&mut budget);
        let wire = aterm_toml::to_string(&carry).expect("writes");
        let carry: FabricCarry = aterm_toml::from_str(&wire).expect("reads back");
        let carried_mark = "inbox-gap: MailGap { from: 42, to: 50, records: None }";
        for (local_id, log_carried) in [(3, false), (4, true)] {
            let (_, successor) = registered_as(&store, local_id);
            successor.timeline.lock().unwrap().log_carried = log_carried;
            successor.fabric.seed_carry(&carry, &successor.timeline);
            assert!(header(&successor).ends_with(" given_up=@42..@50"), "{wire}");
            let want: &[&str] = if log_carried { &[] } else { &[carried_mark] };
            assert_eq!(
                marks(&successor),
                want,
                "a log that is not the carried one marks the give-up its header names, once; \
                 the carried one has its mark (log carried: {log_carried})"
            );
            assert_eq!(tell("gap from=42 to=50"), "OK\n");
            assert_eq!(marks(&successor), want, "a re-tell is a repeat");
        }

        let moved = format!("{}.1", "c".repeat(32));
        assert_eq!(
            cmd_link(
                &store,
                &format!("log {moved} continues=50 was={log}"),
                Some(generation)
            ),
            "OK forgot=1\n"
        );
        assert!(header(&a).ends_with(" given_up=@42..@50"), "below the fork");
        let other = format!("{}.0", "d".repeat(32));
        assert_eq!(
            cmd_link(&store, &format!("log {other}"), Some(generation)),
            "OK forgot=1\n"
        );
        assert!(!header(&a).contains("given_up"), "{}", header(&a));
    });
}

/// B12: `link gap … sid=<sid>` (a floor the bridge's own read of that session's lane met)
/// tells that session alone, in its own header field (`lane_given_up=`) and the mark a node's
/// give-up uses, unless its ring holds mail of the range (a bridge restarted under a running
/// aterm reads behind what the ring was delivered); a broadcast in the range is not mail.
/// `… topic=<t>` names the topic's range on its `topic ls` row and marks it with the topic,
/// unless the session does not hold the topic or its ring holds a broadcast of it in the
/// range; a repeat marks nothing, and a dropped topic or a replaced log forgets it. An unknown
/// session is no one to tell. NC (run): a `note_given_up` that skips the ring check marks the
/// held ranges; one that returns at once marks nothing.
#[test]
fn a_scoped_link_gap_tells_one_session_unless_its_ring_holds_the_range() {
    with_link(|| {
        let store = new_store();
        let (sa, a) = registered_as(&store, 1);
        let (sb, b) = registered_as(&store, 2);
        let generation = next_bridge_generation();
        attach_up(&store, generation);
        let log = format!("{}.0", "c".repeat(32));
        assert_eq!(
            cmd_link(&store, &format!("log {log}"), Some(generation)),
            "OK\n"
        );
        let woke = Mutex::new(Vec::new());
        let tell = |line: &str| {
            cmd_link_waking(&store, line, Some(generation), || {}, &|sid| {
                woke.lock().unwrap().push(sid.as_str().to_string());
            })
        };
        let header = |ctx: &SessionCtx| {
            let out = cmd_inbox(ctx, "--peek --meta");
            out.lines().next().unwrap_or_default().to_string()
        };
        let marks = |ctx: &SessionCtx| -> Vec<String> {
            let tl = ctx.timeline.lock().unwrap();
            tl.facts
                .iter()
                .filter(|f| f.starts_with("inbox-gap: "))
                .cloned()
                .collect()
        };
        let news = |ctx: &SessionCtx| -> String {
            cmd_topic(ctx, "ls")
                .lines()
                .find(|l| l.starts_with("topic news "))
                .unwrap_or_default()
                .to_string()
        };

        assert_eq!(tell(&format!("gap from=10 to=20 sid={sa}")), "OK\n");
        assert!(
            header(&a).ends_with(" lane_given_up=@10..@20"),
            "{}",
            header(&a)
        );
        assert_eq!(
            marks(&a),
            ["inbox-gap: MailGap { from: 10, to: 20, records: None }"]
        );
        assert!(!header(&b).contains("given_up"), "{}", header(&b));
        assert!(marks(&b).is_empty(), "the other session is not told");
        assert_eq!(*woke.lock().unwrap(), [sa.as_str()]);
        assert_eq!(tell(&format!("gap from=10 to=20 sid={sa}")), "OK\n");
        assert_eq!(marks(&a).len(), 1, "a repeat is not marked again");

        assert!(deliver(&store, &sb, 31, "s-peer", "note", "kept").starts_with("OK"));
        assert_eq!(b.fabric.topic_add("news", "@40", &b.timeline), Ok(true));
        assert!(broadcast(&store, &sb, 45, "news", "heard").starts_with("OK"));
        assert_eq!(tell(&format!("gap from=30 to=40 sid={sb}")), "OK\n");
        assert!(
            marks(&b).is_empty() && !header(&b).contains("given_up"),
            "the ring holds mail at 31: {}",
            header(&b)
        );
        assert_eq!(tell(&format!("gap from=40 to=50 sid={sb}")), "OK\n");
        assert!(
            header(&b).ends_with(" lane_given_up=@40..@50"),
            "a broadcast is not mail: {}",
            header(&b)
        );

        assert_eq!(
            tell(&format!("gap from=40 to=50 sid={sb} topic=news")),
            "OK\n"
        );
        assert_eq!(
            news(&b),
            "topic news since=@40",
            "the ring holds the topic's broadcast at 45"
        );
        assert_eq!(
            tell(&format!("gap from=50 to=60 sid={sb} topic=news")),
            "OK\n"
        );
        assert_eq!(news(&b), "topic news since=@40 given_up=@50..@60");
        let topic_mark = "inbox-gap: TopicGap { from: 50, to: 60, topic: \"news\" }";
        assert_eq!(marks(&b).last().map(String::as_str), Some(topic_mark));
        let marked = marks(&b).len();
        assert_eq!(
            tell(&format!("gap from=50 to=60 sid={sb} topic=news")),
            "OK\n"
        );
        assert_eq!(marks(&b).len(), marked, "a repeat is not marked again");
        assert_eq!(
            tell(&format!("gap from=50 to=60 sid={sa} topic=news")),
            "OK\n"
        );
        assert_eq!(marks(&a).len(), 1, "a topic the session does not hold");
        assert_eq!(tell("gap from=50 to=60 sid=s-nobody"), "OK\n");

        assert!(cmd_topic(&*b, "drop news").starts_with("OK news dropped=1"));
        assert_eq!(b.fabric.topic_add("news", "@40", &b.timeline), Ok(true));
        assert_eq!(news(&b), "topic news since=@40", "the drop forgot it");
        assert_eq!(
            tell(&format!("gap from=50 to=60 sid={sb} topic=news")),
            "OK\n"
        );
        assert_eq!(news(&b), "topic news since=@40 given_up=@50..@60");
        let other = format!("{}.0", "d".repeat(32));
        assert_eq!(
            cmd_link(&store, &format!("log {other}"), Some(generation)),
            "OK forgot=1\n"
        );
        assert_eq!(news(&b), "topic news since=@40", "a replaced log forgot it");

        for bad in [
            "gap from=1 to=2 topic=news",
            "gap from=1 to=2 sid=s-a sid=s-b",
            "gap from=1 to=2 sid=",
            "gap from=1 to=2 sid=s-a topic=News!",
            "gap from=1 to=2 sid=s-a topic=news topic=news",
        ] {
            assert!(
                cmd_link(&store, bad, Some(generation)).starts_with("ERR usage"),
                "{bad}"
            );
        }
    });
}

/// R5: the node's own count of what retention gave up of its unread mail, which `fabric
/// status` names (`mail_gaps=`, [`fabric_gap_facts`]) for `aterm fabric`'s WARNINGS. Every
/// node range the current bridge tells counts once: a repeat, and a range told again over the
/// newest it overlaps (a new subscription's notice, its count the whole range's) widen the
/// newest and count nothing; a lane's or a topic's range and a stale bridge's count nothing;
/// a range with no session to tell still counts, and a new bridge keeps the count. The newest
/// range is named while its offsets name the bridge's log (that log, or one continuing it
/// past the range's end), its count where the broker gave one; a replaced log hides it and
/// keeps the count. NC (run): a `NodeGaps::note` that counts every tell fails the repeat; one
/// that returns at once fails the first tell; a `NodeGaps::log_moved` that keeps the newest
/// over any move fails the replaced log.
#[test]
fn the_nodes_given_up_mail_is_counted_once_per_range_for_fabric_status() {
    with_link(|| {
        let store = new_store();
        let (sa, a) = registered_as(&store, 1);
        let generation = next_bridge_generation();
        attach_up(&store, generation);
        let log = format!("{}.0", "c".repeat(32));
        let tell = |line: &str| cmd_link(&store, line, Some(generation));
        assert_eq!(tell(&format!("log {log}")), "OK\n");
        assert_eq!(fabric_gap_facts(), "", "nothing told, nothing said");

        assert_eq!(tell("gap from=10 to=42 records=3"), "OK\n");
        let first = " mail_gaps=1 mail_gap=@10..@42 mail_gap_count=3";
        assert_eq!(fabric_gap_facts(), first);
        for repeat in ["gap from=10 to=42", "gap to=42 from=10 records=3"] {
            assert_eq!(tell(repeat), "OK\n", "{repeat}");
            assert_eq!(fabric_gap_facts(), first, "a repeat: {repeat}");
        }
        assert_eq!(a.fabric.topic_add("news", "@0", &a.timeline), Ok(true));
        for scoped in [
            format!("gap from=50 to=60 sid={sa}"),
            format!("gap from=50 to=60 sid={sa} topic=news"),
        ] {
            assert_eq!(tell(&scoped), "OK\n", "{scoped}");
            assert_eq!(fabric_gap_facts(), first, "a session's own: {scoped}");
        }
        let ghost = next_bridge_generation();
        assert_eq!(
            cmd_link(&store, "gap from=70 to=80", Some(ghost)),
            "OK stale=1\n"
        );
        assert_eq!(fabric_gap_facts(), first, "a stale bridge's range");

        assert_eq!(tell("gap from=8 to=50 records=5"), "OK\n");
        assert_eq!(
            fabric_gap_facts(),
            " mail_gaps=1 mail_gap=@8..@50 mail_gap_count=5",
            "a new subscription's range over the newest is the same range, widened"
        );
        assert_eq!(tell("gap from=20 to=60"), "OK\n");
        assert_eq!(
            fabric_gap_facts(),
            " mail_gaps=1 mail_gap=@8..@60",
            "one that covers neither: no count"
        );

        let nobody = new_store();
        assert_eq!(
            cmd_link(&nobody, "gap from=70 to=75 records=1", Some(generation)),
            "OK\n"
        );
        assert_eq!(
            fabric_gap_facts(),
            " mail_gaps=2 mail_gap=@70..@75 mail_gap_count=1",
            "a range no session was there to be told of"
        );
        assert_eq!(tell("gap from=75 to=75"), "OK\n");
        assert_eq!(
            fabric_gap_facts(),
            " mail_gaps=2 mail_gap=@70..@75 mail_gap_count=1"
        );

        let next = next_bridge_generation();
        attach_up(&store, next);
        assert_eq!(
            fabric_gap_facts(),
            " mail_gaps=2 mail_gap=@70..@75 mail_gap_count=1",
            "a new bridge keeps the count"
        );
        let moved = format!("{}.1", "c".repeat(32));
        assert_eq!(
            cmd_link(
                &store,
                &format!("log {moved} continues=75 was={log}"),
                Some(next)
            ),
            "OK forgot=1\n"
        );
        assert_eq!(
            fabric_gap_facts(),
            " mail_gaps=2 mail_gap=@70..@75 mail_gap_count=1",
            "a log continuing past the range still names it"
        );
        let other = format!("{}.0", "d".repeat(32));
        assert_eq!(
            cmd_link(&store, &format!("log {other}"), Some(next)),
            "OK forgot=1\n"
        );
        assert_eq!(
            fabric_gap_facts(),
            " mail_gaps=2",
            "a replaced log does not"
        );
        assert_eq!(cmd_link(&store, "gap from=70 to=75", Some(next)), "OK\n");
        assert_eq!(
            fabric_gap_facts(),
            " mail_gaps=3 mail_gap=@70..@75",
            "the same offsets on another log are another range"
        );
    });
}

/// R5 review: the node's count keeps its newest range across a move of the bus log exactly
/// as each session keeps its `given_up=` ([`Inbox::forget_bus_offsets`]): a generation that
/// continues the log past the range's end keeps it, through any number of such moves, so a
/// new bridge's subscription telling it again, with no count, after a broker restart moved
/// the log, is that range told again ([`GivenUp::repeated_by`]): one range, its count kept,
/// one `mail-gap` mark. A move whose fork falls inside the range forgets it in both, and the
/// range told after it is a new one in both. NC (run): a `NodeGaps` whose newest stays tied
/// to the log it was told on (the branch's first shape), and one whose `log_moved` forgets
/// it at every move, each count the re-tell after the first move again and drop its count;
/// a `log_moved` that keeps it over any move fails the fork inside the range.
#[test]
fn a_node_range_told_again_on_a_log_continuing_past_it_is_the_same_range() {
    with_link(|| {
        let store = new_store();
        let (_, a) = registered_as(&store, 1);
        let generation = next_bridge_generation();
        attach_up(&store, generation);
        let tell = |line: &str| cmd_link(&store, line, Some(generation));
        let header = || {
            let out = cmd_inbox(&*a, "--peek --meta");
            out.lines().next().unwrap_or_default().to_string()
        };
        let marks = || {
            let tl = a.timeline.lock().unwrap();
            tl.facts
                .iter()
                .filter(|f| f.starts_with("inbox-gap: "))
                .count()
        };
        let log = |g: u32| format!("{}.{g}", "c".repeat(32));
        let moved = |g: u32, fork: u64| {
            assert_eq!(
                tell(&format!(
                    "log {} continues={fork} was={}",
                    log(g),
                    log(g - 1)
                )),
                "OK forgot=1\n",
                "the move to {}",
                log(g)
            );
        };
        assert_eq!(tell(&format!("log {}", log(0))), "OK\n");
        assert_eq!(tell("gap from=70 to=75 records=1"), "OK\n");
        let once = " mail_gaps=1 mail_gap=@70..@75 mail_gap_count=1";
        let held = " given_up=@70..@75 given_up_count=1";
        assert_eq!(fabric_gap_facts(), once);

        moved(1, 100);
        assert_eq!(tell("gap from=70 to=75"), "OK\n");
        assert_eq!(
            fabric_gap_facts(),
            once,
            "told again on a log continuing past it"
        );
        assert!(header().ends_with(held), "{}", header());
        assert_eq!(marks(), 1);

        moved(2, 110);
        moved(3, 120);
        assert_eq!(fabric_gap_facts(), once, "two moves past it, nothing told");
        assert!(header().ends_with(held), "{}", header());
        assert_eq!(tell("gap from=70 to=75"), "OK\n");
        assert_eq!(fabric_gap_facts(), once, "and told again after them");
        assert_eq!(marks(), 1);

        moved(4, 72);
        assert_eq!(
            fabric_gap_facts(),
            " mail_gaps=1",
            "a fork inside the range"
        );
        assert!(!header().contains(" given_up="), "{}", header());
        assert_eq!(tell("gap from=70 to=75"), "OK\n");
        assert_eq!(
            fabric_gap_facts(),
            " mail_gaps=2 mail_gap=@70..@75",
            "told after a fork inside it: a new range"
        );
        assert!(header().ends_with(" given_up=@70..@75"), "{}", header());
        assert_eq!(marks(), 2);
    });
}

/// B12 review: a node's give-up (`link gap`, no `sid=`) and one session's lane give-up
/// (`… sid=<sid>`) are kept apart, so each range is marked once however the two interleave:
/// astream tells the node's range again on every new inbox-group subscription until a record
/// is committed past it, and a new bridge's refill tells the lane's again. The header names
/// each in its own field (`given_up=` with the node's count, `lane_given_up=`), so a re-tell
/// of one never puts the other out of it; a seamless update carries both, a successor whose
/// log started over marking each once there and a re-tell of either marking nothing; a log
/// replaced past them forgets both. NC (run): a lane's range kept in the node's one slot (the
/// branch's first shape) marks the node's range again after the lane's and drops the node's
/// from the carry.
#[test]
fn a_node_and_a_lane_give_up_are_marked_once_each_and_carried_apart() {
    with_link(|| {
        let store = new_store();
        let (sa, a) = registered_as(&store, 1);
        let (_, b) = registered_as(&store, 2);
        let generation = next_bridge_generation();
        attach_up(&store, generation);
        let log = format!("{}.0", "c".repeat(32));
        let tell = |line: &str| cmd_link(&store, line, Some(generation));
        assert_eq!(tell(&format!("log {log}")), "OK\n");
        let header = |ctx: &SessionCtx| {
            let out = cmd_inbox(ctx, "--peek --meta");
            out.lines().next().unwrap_or_default().to_string()
        };
        let marks = |ctx: &SessionCtx| -> Vec<String> {
            let tl = ctx.timeline.lock().unwrap();
            tl.facts
                .iter()
                .filter(|f| f.starts_with("inbox-gap: "))
                .cloned()
                .collect()
        };
        let node = "gap from=10 to=20 records=3".to_string();
        let lane = format!("gap from=30 to=40 sid={sa}");
        let node_mark = "inbox-gap: MailGap { from: 10, to: 20, records: Some(3) }";
        let lane_mark = "inbox-gap: MailGap { from: 30, to: 40, records: None }";
        let both = " given_up=@10..@20 given_up_count=3 lane_given_up=@30..@40";

        for line in [&node, &node, &lane, &node, &lane, &node] {
            assert_eq!(tell(line), "OK\n", "{line}");
        }
        assert_eq!(tell("gap from=10 to=20"), "OK\n", "re-told with no count");
        assert_eq!(marks(&a), [node_mark, lane_mark], "each range once");
        assert!(header(&a).ends_with(both), "{}", header(&a));
        assert_eq!(marks(&b), [node_mark], "the lane's range is a's alone");
        assert!(
            header(&b).ends_with(" pending=0 given_up=@10..@20 given_up_count=3"),
            "{}",
            header(&b)
        );

        let mut budget = CARRY_MAIL_BYTES;
        let carry = a.fabric.handoff_carry(&mut budget);
        let wire = aterm_toml::to_string(&carry).expect("writes");
        let carry: FabricCarry = aterm_toml::from_str(&wire).expect("reads back");
        for (local_id, log_carried) in [(3, false), (4, true)] {
            let (sid, successor) = registered_as(&store, local_id);
            successor.timeline.lock().unwrap().log_carried = log_carried;
            successor.fabric.seed_carry(&carry, &successor.timeline);
            assert!(header(&successor).ends_with(both), "{wire}");
            let want: &[&str] = if log_carried {
                &[]
            } else {
                &[node_mark, lane_mark]
            };
            assert_eq!(
                marks(&successor),
                want,
                "a log that is not the carried one marks each range once (log carried: \
                 {log_carried})"
            );
            for line in [format!("gap from=30 to=40 sid={sid}"), node.clone()] {
                assert_eq!(tell(&line), "OK\n", "{line}");
            }
            assert_eq!(marks(&successor), want, "a re-tell of either is a repeat");
            assert!(header(&successor).ends_with(both), "{}", header(&successor));
        }

        let (sc, c) = registered_as(&store, 5);
        let lane = format!("gap from=10 to=20 sid={sc}");
        for line in [&lane, &"gap from=5 to=30 records=2".to_string(), &lane] {
            assert_eq!(tell(line), "OK\n", "{line}");
        }
        assert_eq!(
            marks(&c),
            [
                "inbox-gap: MailGap { from: 10, to: 20, records: None }",
                "inbox-gap: MailGap { from: 5, to: 30, records: Some(2) }",
            ],
            "the lane's range told before the node's is not marked again after it"
        );
        assert!(
            header(&c).ends_with(" given_up=@5..@30 given_up_count=2 lane_given_up=@10..@20"),
            "{}",
            header(&c)
        );

        let other = format!("{}.0", "d".repeat(32));
        assert_eq!(tell(&format!("log {other}")), "OK forgot=1\n");
        assert!(!header(&c).contains("given_up"), "{}", header(&c));
    });
}

#[test]
fn a_broadcast_on_a_topic_the_session_does_not_hold_is_refused() {
    let store = new_store();
    let (sid, ctx) = registered(&store);
    assert_eq!(
        broadcast(&store, &sid, 10, "news", "unasked"),
        "ERR not-subscribed\n",
        "an empty set receives nothing"
    );
    assert_eq!(
        ctx.fabric.topic_add("news", "head", &ctx.timeline),
        Ok(true)
    );
    assert_eq!(broadcast(&store, &sid, 11, "news", "wanted"), "OK 1\n");
    assert_eq!(
        broadcast(&store, &sid, 12, "other", "elsewhere"),
        "ERR not-subscribed\n",
        "one topic held is not every topic"
    );
    assert!(ctx.fabric.topic_drop("news", &ctx.timeline));
    assert_eq!(
        broadcast(&store, &sid, 13, "news", "after-the-drop"),
        "ERR not-subscribed\n"
    );
    assert_eq!(
        broadcast(&store, &sid, 11, "news", "wanted"),
        "OK 1\n",
        "a redelivered offset still answers the id it first got"
    );
    assert_eq!(
        deliver(&store, &sid, 14, "h-a", "task", "addressed"),
        "OK 2\n",
        "the set gates broadcasts, not addressed mail"
    );
    let header = cmd_inbox(&ctx, "--peek");
    assert!(
        header.starts_with("OK 2 ") && header.contains(" dropped=0 "),
        "the two taken rows and no loss counted for the refusals: {header}"
    );
    assert_eq!(ctx.fabric.topic_add("news", "@13", &ctx.timeline), Ok(true));
    assert_eq!(
        broadcast(&store, &sid, 13, "news", "after-the-drop"),
        "OK 3\n"
    );
}

#[test]
fn the_watermarks_move_only_where_they_should() {
    let store = new_store();
    let (sid, ctx) = registered(&store);
    for off in 1..=3 {
        deliver(&store, &sid, off, "h-a", "task", "x");
    }
    assert!(cmd_inbox(&ctx, "--peek").starts_with("OK 3 hold=0 holder=- seen=0 "));
    assert!(cmd_inbox(&ctx, "--peek").starts_with("OK 3 hold=0 holder=- seen=0 "));
    {
        let inbox = ctx.fabric.lock();
        assert!(inbox.rows.iter().all(|r| !r.listed), "--peek listed a row");
    }
    assert!(cmd_inbox(&ctx, "").contains(" seen=0 "));
    {
        let inbox = ctx.fabric.lock();
        assert!(inbox.rows.iter().all(|r| r.listed));
    }
    assert_eq!(cmd_inbox_seen(&ctx, "2 handled"), "OK seen=2\n");
    assert_eq!(
        cmd_inbox_seen(&ctx, "1 deferred"),
        "OK seen=2\n",
        "the handled watermark never goes backwards"
    );
    assert_eq!(cmd_inbox_seen(&ctx, "99"), "ERR no such message\n");
    assert_eq!(cmd_inbox_seen(&ctx, "2 shrugged"), INBOX_USAGE);
    assert!(cmd_inbox(&ctx, "--peek").contains(" seen=2 "));
    assert!(cmd_topic(&ctx, "ls").starts_with("OK 0 seen=2\n"));
    assert_eq!(rows(&cmd_inbox(&ctx, "since=2 --peek")).len(), 1);
    let one = cmd_inbox(&ctx, "1 --peek");
    assert!(one.starts_with("OK 1 "), "{one}");
    assert!(rows(&one)[0].starts_with("msg 3 "), "newest kept: {one}");
}

#[test]
fn a_long_body_is_cut_in_the_row_and_whole_through_inbox_get() {
    let store = new_store();
    let (sid, ctx) = registered(&store);
    let body = "z".repeat(TEXT_PREVIEW_MAX + 40);
    assert_eq!(deliver(&store, &sid, 5, "h-a", "report", &body), "OK 1\n");
    let row = rows(&cmd_inbox(&ctx, "--peek"))[0].to_string();
    assert!(row.contains(&format!(" len={} ", body.len())), "{row}");
    assert!(row.contains(" more=1 "), "{row}");
    let text = row.split(" text=").nth(1).expect("a text field");
    assert_eq!(text.len(), TEXT_PREVIEW_MAX, "cut at the preview bound");
    let meta = rows(&cmd_inbox(&ctx, "--peek --meta"))[0].to_string();
    assert!(!meta.contains(" text="), "--meta carries no body: {meta}");
    assert!(meta.contains(" more=1"), "{meta}");
    let full = cmd_inbox_get(&ctx, "1");
    assert_eq!(full, format!("OK {}\n{body}", body.len()));
    assert_eq!(cmd_inbox_get(&ctx, "77"), "ERR no such message\n");
    assert!(cmd_inbox(&ctx, "--peek").contains(" seen=0 "));
}

/// An imported direct fleet message (§11 C6) closes on the session log by its own fact, a
/// reply's or a retirement's, as with the mesh off: its `inbox seen` names no bus offset
/// and moves only the bus messages the watermark passed.
#[cfg(unix)]
#[test]
fn a_fleet_rows_seen_names_no_offset_and_closes_only_the_bus_rows_it_passed() {
    let store = new_store();
    let (sid, ctx) = registered(&store);
    let deliver = |off: u64| {
        let line = format!("{sid} off={off} from=h-a kind=task trust=human text=BUS");
        cmd_deliver_waking(&store, &line, &|_| {})
    };
    let import = |id: u64| {
        ctx.fabric
            .lock()
            .import_fleet(id, &format!("n-{}", "a".repeat(16)), "ask", "DIRECT");
    };
    let seen = |rest: &str| {
        ctx.timeline.lock().unwrap().facts.clear();
        assert!(cmd_inbox_seen(&ctx, rest).starts_with("OK seen="), "{rest}");
        let facts = ctx.timeline.lock().unwrap().facts.clone();
        assert_eq!(facts.len(), 1, "{rest}: {facts:?}");
        facts[0].clone()
    };
    assert_eq!(deliver(7), "OK 1\n");
    import(4);
    // Control: a bus message's seen names its offset and closes it.
    let bus = seen("1");
    assert!(
        bus.contains("off: Some(7)") && bus.contains("moved: true"),
        "{bus}"
    );
    let direct = seen("2 handled");
    assert!(
        direct.contains("off: None")
            && direct.contains("verdict: Some(\"handled\")")
            && direct.contains("moved: false"),
        "the direct message waits for its own close: {direct}"
    );
    assert_eq!(deliver(8), "OK 3\n");
    import(5);
    let passed = seen("4");
    assert!(
        passed.contains("off: None") && passed.contains("moved: true"),
        "the bus message the watermark passed is closed: {passed}"
    );
}

#[test]
fn every_fabric_site_hands_its_facts_to_the_one_hook() {
    use std::time::Duration;
    let store = new_store();
    let (sid, ctx) = registered(&store);
    let woke = std::cell::Cell::new(0u32);
    let wake = |s: &SessionId| {
        assert_eq!(s.as_str(), sid, "the session's own id");
        woke.set(woke.get() + 1);
    };
    let line = format!("{sid} off=7 from=h-a kind=task trust=human text=SECRET%20BODY");
    assert_eq!(cmd_deliver_waking(&store, &line, &wake), "OK 1\n");
    assert_eq!(cmd_deliver_waking(&store, &line, &wake), "OK 1\n");
    let watcher = test_support::Subscription::single(&ctx.watchers);
    let _ = cmd_post(&ctx, "to=@s-peer kind=ask --wait=0 SECRET-POST", None);
    assert!(watcher.wait(Duration::ZERO), "the post's record rang");
    let landed = format!("{sid} landed=1 off=9");
    assert_eq!(cmd_deliver_waking(&store, &landed, &wake), "OK\n");
    assert_eq!(cmd_deliver_waking(&store, &landed, &wake), "OK\n");
    let _ = watcher.wait(Duration::ZERO);
    let _ = cmd_inbox_seen(&ctx, "1 handled");
    assert!(watcher.wait(Duration::ZERO), "the seen's record rang");
    let _ = cmd_inbox_seen(&ctx, "1 handled");
    let hold =
        |rest: &str| cmd_hold_waking(&store, &format!("{sid} {rest}"), HoldIssuer::Owner, &wake);
    assert_eq!(hold("on reason=demo"), "OK hold=1\n");
    assert_eq!(hold("on reason=demo"), "OK hold=1\n");
    assert_eq!(hold("off"), "OK hold=0\n");
    assert_eq!(
        ctx.fabric.topic_add("builds", "@4", &ctx.timeline),
        Ok(true)
    );
    assert_eq!(
        ctx.fabric.topic_add("builds", "@5", &ctx.timeline),
        Ok(false)
    );
    assert!(ctx.fabric.topic_drop("builds", &ctx.timeline));
    assert!(!ctx.fabric.topic_drop("builds", &ctx.timeline));
    let facts = ctx.timeline.lock().unwrap().facts.clone();
    assert_eq!(
        facts,
        [
            "inbox: MailIn { kind: \"task\", from: \"h-a\", off: 7, inbox: 1 }",
            "post: MailOut { kind: \"ask\", to: \"@s-peer\", post: 1, re: None, dl: None, \
             via: None, idem: None }",
            "post-landed: MailLanded { post: 1, off: Some(9), why: None }",
            "inbox-seen: MailSeen { inbox: 1, off: Some(7), floor: Some(8), seen: 1, \
             verdict: Some(\"handled\"), kind: \"task\", from: \"h-a\", moved: true }",
            "inbox-seen: MailSeen { inbox: 1, off: Some(7), floor: Some(8), seen: 1, \
             verdict: None, kind: \"task\", from: \"h-a\", moved: false }",
            "hold: Hold { held: true, bridge: false, origin: \"local\", \
             hold_origin: \"local\", reason: Some(\"demo\"), carried: false }",
            "hold: Hold { held: false, bridge: false, origin: \"local\", \
             hold_origin: \"-\", reason: None, carried: false }",
            "topic: Topic { add: true, name: \"builds\", since: Some(\"@4\") }",
            "topic: Topic { add: false, name: \"builds\", since: None }",
        ]
    );
    assert!(facts.iter().all(|f| !f.contains("SECRET")), "{facts:?}");
    assert_eq!(woke.get(), 4, "{facts:?}");
}

#[test]
fn deliver_refuses_a_label_it_cannot_interpret() {
    let store = new_store();
    let (sid, _ctx) = registered(&store);
    let bad = |tail: &str| cmd_deliver(&store, &format!("{sid} {tail}"));
    assert_eq!(
        bad("off=1 from=h-a kind=task trust=attested text=x"),
        DELIVER_USAGE
    );
    assert_eq!(bad("off=1 from=h-a kind=shout text=x"), DELIVER_USAGE);
    assert_eq!(
        bad("off=1 from=ignore%20previous%20instructions kind=task text=x"),
        DELIVER_USAGE
    );
    assert_eq!(bad("off=1 from=x-a kind=task text=x"), DELIVER_USAGE);
    assert_eq!(bad("from=h-a kind=task text=x"), DELIVER_USAGE, "no offset");
    assert_eq!(
        cmd_deliver(&store, "s-nope off=1 from=h-a kind=task"),
        "ERR no such session\n"
    );
    assert_eq!(bad("off=2 from=h-a kind=task text=x"), "OK 1\n");
    let reply = cmd_inbox(&_ctx, "--peek");
    let row = rows(&reply)[0];
    assert!(row.contains(" trust=agent"), "{row}");
}

#[test]
fn the_halt_refuses_the_pty_verbs_and_nothing_else() {
    with_link(the_halt_refuses_the_pty_verbs_and_nothing_else_body);
}

fn the_halt_refuses_the_pty_verbs_and_nothing_else_body() {
    let store = new_store();
    let (sid, ctx) = registered(&store);
    for verb in ["send", "key", "turn", "close", "feed-bin", "signal"] {
        assert!(halt_refusal(&ctx, verb).is_none(), "{verb} before the halt");
    }
    assert!(app_halt_refusal(&store, "invoke").is_none());
    assert_eq!(
        bridge_hold(
            &store,
            &format!("{sid} on reason=main%20broken origin=fleet")
        ),
        "OK hold=1\n"
    );
    for verb in [
        "send",
        "key",
        "ctrl",
        "feed",
        "feed-bin",
        "paste",
        "paste-bin",
        "mouse",
        "pointer",
        "resize",
        "focus",
        "signal",
        "turn",
        "close",
        "invoke",
        "hwkey",
        "tab",
    ] {
        assert_eq!(
            halt_refusal(&ctx, verb).as_deref(),
            Some("ERR halted reason=main%20broken origin=fleet\n"),
            "{verb} must be refused under a halt"
        );
    }
    assert_eq!(
        app_halt_refusal(&store, "hwkey").as_deref(),
        Some("ERR halted reason=main%20broken origin=fleet\n"),
        "hwkey posts a real NSEvent on the physical-keypress path: a halt that let it \
         through would stop the polite routes and leave the indistinguishable one"
    );
    assert_eq!(
        app_halt_refusal(&store, "invoke").as_deref(),
        Some("ERR halted reason=main%20broken origin=fleet\n"),
        "the App lane is where `invoke` is actually refused"
    );
    assert!(app_halt_refusal(&store, "hover").is_none());
    for verb in [
        "post",
        "inbox",
        "inbox seen",
        "meta",
        "lease",
        "text",
        "status",
        "await",
        "subscribe",
        "spawn",
    ] {
        assert!(
            halt_refusal(&ctx, verb).is_none(),
            "{verb} must stay answerable under a halt"
        );
    }
    assert_eq!(bridge_hold(&store, &format!("{sid} off")), "OK hold=0\n");
    assert!(halt_refusal(&ctx, "turn").is_none(), "the halt lifted");
    assert_eq!(bridge_hold(&store, &format!("{sid} sideways")), HOLD_USAGE);
    assert_eq!(
        bridge_hold(&store, &format!("{sid} on origin=nowhere")),
        HOLD_USAGE
    );
    assert_eq!(bridge_hold(&store, "s-nope on"), "ERR no such session\n");
}

#[test]
fn a_halt_reason_cannot_carry_anything_but_pct_ascii() {
    assert_eq!(reason_token(""), "-");
    assert_eq!(reason_token("   "), "-");
    assert_eq!(reason_token("main%20broken"), "main%20broken");
    assert_eq!(
        reason_token("ok\nERR forged"),
        "okERRforged",
        "a newline can never split one reply into two"
    );
    assert_eq!(reason_token("caf\u{e9} \u{202e}rap"), "cafrap");
    let long = "x".repeat(REASON_MAX + 50);
    assert_eq!(reason_token(&long).len(), REASON_MAX);
    let straddle = format!("{}%41", "y".repeat(REASON_MAX - 2));
    let out = reason_token(&straddle);
    assert!(!out.ends_with('%'), "{out}");
    assert_eq!(out.len(), REASON_MAX - 2, "{out}");
    assert_eq!(reason_token("100%sure"), "100%25sure");
    assert_eq!(reason_token("a%g"), "a%25g");
    assert_eq!(reason_token("a%2"), "a");
    assert_eq!(reason_token("a%"), "a");
    assert_eq!(reason_token("%%41%4"), "%25%41");
    let flood = reason_token(&"%".repeat(500));
    assert_eq!(flood, "%25".repeat(REASON_MAX / 3), "a flood is escapes");
    for raw in ["%", "a%", "a%4", "%zz%41", &"%".repeat(500), &straddle] {
        let out = reason_token(raw);
        let b = out.as_bytes();
        let escapes = b.iter().filter(|&&c| c == b'%').count();
        assert!(
            (0..b.len()).filter(|&i| b[i] == b'%').all(|i| {
                b.get(i + 1).is_some_and(u8::is_ascii_hexdigit)
                    && b.get(i + 2).is_some_and(u8::is_ascii_hexdigit)
            }),
            "{raw:?}: a `%` that starts no escape in {out:?}"
        );
        assert!(
            out.len() <= REASON_MAX && escapes <= REASON_MAX / 3,
            "{out}"
        );
    }
}

#[test]
fn losing_the_bridge_leaves_every_session_running_with_an_advisory() {
    with_link(|| {
        assert!(!holds_on_bridge_lost(), "the default halts nothing");
        let store = new_store();
        let (sid, ctx) = registered(&store);
        let (held_sid, held_ctx) = registered_as(&store, 2);
        let untouched = test_handle(3);
        let untouched_ctx = untouched.ctx.clone();
        store
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .register(untouched);
        let generation = next_bridge_generation();
        attach_up(&store, generation);
        assert_eq!(deliver(&store, &sid, 7, "h-a", "task", "go"), "OK 1\n");
        assert_eq!(
            bridge_hold(&store, &format!("{held_sid} on reason=person")),
            "OK hold=1\n"
        );

        assert_eq!(bridge_lost(&store, generation), 2, "two governed sessions");
        assert_eq!(fabric_state(), "disconnected");
        assert!(halt_refusal(&ctx, "turn").is_none(), "nothing halted");
        assert!(ctx.fabric.hold().is_none());
        assert_eq!(
            ctx.fabric.hold_advisory().map(|h| (h.reason, h.origin)),
            Some(("fabric-lost".to_string(), "fleet".to_string()))
        );
        assert!(
            ctx.timeline
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .since(None)
                .any(|e| e.kind == "hold-advisory"),
            "recorded on the timeline"
        );
        assert_eq!(
            halt_refusal(&held_ctx, "turn").as_deref(),
            Some("ERR halted reason=person origin=fleet\n")
        );
        assert!(held_ctx.fabric.hold_advisory().is_none());
        assert!(halt_refusal(&untouched_ctx, "turn").is_none());
        assert!(untouched_ctx.fabric.hold_advisory().is_none());
        let next = next_bridge_generation();
        attach_up(&store, next);
        assert!(ctx.fabric.hold_advisory().is_none());
        assert!(halt_refusal(&ctx, "turn").is_none());
    });
}

#[test]
fn a_bye_is_taken_only_from_the_bridge_that_owns_the_link() {
    with_link(|| {
        let store = new_store();
        let old = next_bridge_generation();
        bridge_attached(old);
        let live = next_bridge_generation();
        bridge_attached(live);
        let before = byes_taken();
        assert_eq!(cmd_link(&store, "bye quit", Some(old)), "OK stale=1\n");
        assert_eq!(cmd_link(&store, "bye quit", None), "OK stale=1\n");
        assert!(!await_bye_taken(before, std::time::Duration::ZERO));
        assert_eq!(cmd_link(&store, "bye handoff", Some(live)), "OK\n");
        assert!(await_bye_taken(before, std::time::Duration::ZERO));
        for bad in ["bye", "bye exit", "bye quit now"] {
            assert_eq!(cmd_link(&store, bad, Some(live)), LINK_USAGE, "{bad}");
        }
    });
}

#[test]
fn the_link_is_the_bridges_to_report_and_stalled_refuses_a_wait_at_once() {
    with_link(|| {
        let store = new_store();
        let (_sid, ctx) = registered(&store);
        let generation = next_bridge_generation();
        bridge_attached(generation);
        assert_eq!(fabric_state(), "stalled");
        assert_eq!(
            fabric_link_facts(),
            (LINK_STARTING.to_string(), None, None),
            "before the first report: no ack, no age"
        );
        assert_eq!(fabric_status_tail(), "fabric_rtt_ms=- fabric_link_age_ms=-");

        assert_eq!(cmd_link(&store, "up rtt=3", Some(generation)), "OK\n");
        assert_eq!(fabric_state(), "connected");
        let (reason, rtt, age) = fabric_link_facts();
        assert_eq!((reason.as_str(), rtt), ("-", Some(3)));
        assert!(
            age.is_some_and(
                |a| std::time::Duration::from_millis(a) < aterm_spec::wait::MUST_HAPPEN
            ),
            "a fresh ack: {age:?}"
        );
        assert!(
            fabric_status_tail().starts_with("fabric_rtt_ms=3 fabric_link_age_ms="),
            "{}",
            fabric_status_tail()
        );
        assert_eq!(
            cmd_post(&ctx, "to=h-andrew kind=ask --wait=0 up?", None),
            "ERR timeout id=1\n"
        );

        assert_eq!(
            cmd_link(&store, "down reason=refused", Some(generation)),
            "OK\n"
        );
        assert_eq!(fabric_state(), "stalled");
        let (reason, rtt, _) = fabric_link_facts();
        assert_eq!(
            (reason.as_str(), rtt),
            ("refused", Some(3)),
            "the reason is the bridge's word; the last rtt is kept as history"
        );
        let started = std::time::Instant::now();
        assert_eq!(
            cmd_post(&ctx, "to=h-andrew kind=ask --wait=600000 down?", None),
            "ERR fabric stalled id=2 queued=1\n"
        );
        assert!(
            started.elapsed() < aterm_spec::wait::MUST_HAPPEN,
            "a stalled wait is refused at once, not after its timeout: {:?}",
            started.elapsed()
        );

        for bad in [
            "",
            "up",
            "up rtt=x",
            "down",
            "down rtt=1",
            "sideways",
            "up rtt=1 reason=x",
        ] {
            assert_eq!(
                cmd_link(&store, bad, Some(generation)),
                LINK_USAGE,
                "{bad:?}"
            );
        }
        assert_eq!(
            cmd_link(&store, "down reason=NO%20ACK", Some(generation)),
            "OK\n"
        );
        assert_eq!(
            fabric_link_facts().0,
            "20",
            "a reason is lowercase ascii, digits and '-' only"
        );
        assert_eq!(
            cmd_link(&store, "down reason=!!!", Some(generation)),
            "OK\n"
        );
        assert_eq!(fabric_link_facts().0, "unknown");
    });
}

/// `fabric status` reads its state and its reason as one moment: a `stalled` line names why,
/// a `connected` one names no reason, whatever the bridge reports beside the read (a link
/// that flaps while its broker shuts down). Read apart, a `stalled` came with the `-` of the
/// link that was up a moment before (`link_state`'s killed broker, aterm's fabric world).
/// NEGATIVE CONTROL: load `LINK.state` before taking `LINK.link` in [`fabric_link_status`]:
/// the reader sees a torn line within its first move or two in each of ten runs (2026-11-03),
/// so the 500 `MOVES` leave a wide margin.
#[test]
fn fabric_status_reads_its_state_and_reason_as_one_moment() {
    with_link(|| {
        let store = new_store();
        let generation = next_bridge_generation();
        bridge_attached(generation);
        assert_eq!(
            fabric_link_status(),
            ("stalled", LINK_STARTING.to_string(), None, None)
        );
        assert_eq!(cmd_link(&store, "up rtt=2", Some(generation)), "OK\n");
        let (state, reason, rtt, _) = fabric_link_status();
        assert_eq!((state, reason.as_str(), rtt), ("connected", "-", Some(2)));
        assert_eq!(
            cmd_link(&store, "down reason=closed", Some(generation)),
            "OK\n"
        );
        let (state, reason, rtt, _) = fabric_link_status();
        assert_eq!(
            (state, reason.as_str(), rtt),
            ("stalled", "closed", Some(2))
        );

        const MOVES: usize = 500;
        let done = std::sync::atomic::AtomicBool::new(false);
        let torn = std::thread::scope(|scope| {
            let reader = scope.spawn(|| {
                let (mut torn, mut moves, mut last) = (Vec::new(), 0, "");
                while moves < MOVES && !done.load(Ordering::Acquire) {
                    let (state, reason, _, _) = fabric_link_status();
                    if (state == "stalled") == (reason == "-") && torn.len() < 4 {
                        torn.push((state, reason));
                    }
                    moves += usize::from(state != last);
                    last = state;
                }
                done.store(true, Ordering::Release);
                torn
            });
            let mut refused = None;
            while !done.load(Ordering::Acquire) {
                for line in ["up rtt=2", "down reason=closed"] {
                    let reply = cmd_link(&store, line, Some(generation));
                    if reply != "OK\n" {
                        refused = Some(reply);
                        done.store(true, Ordering::Release);
                    }
                }
            }
            (reader.join().expect("the reader"), refused)
        });
        assert_eq!(torn, (Vec::<(&str, String)>::new(), None), "a torn line");
        bridge_lost(&store, generation);
        let (state, reason, _, _) = fabric_link_status();
        assert_eq!((state, reason.as_str()), ("disconnected", LINK_BRIDGE_LOST));
    });
}

/// A link read or write from outside every link section holds [`LINK_SECTION`] from its
/// check to its end: no section opens between them, resets the link and then takes the
/// write into its own test.
#[test]
fn a_link_write_holds_the_section_lock_from_its_check_to_its_write() {
    let held = with_link_reset(|| unless_another_link_section(|| LINK_SECTION.try_lock().is_err()));
    assert_eq!(
        held,
        Some(true),
        "a link write a section can open under between its check and its write"
    );
}

#[test]
fn a_bridge_that_owes_an_answer_reads_stale_and_a_quiet_one_does_not() {
    with_link(|| {
        let store = new_store();
        let (_sid, ctx) = registered(&store);
        let generation = next_bridge_generation();
        bridge_attached(generation);
        assert_eq!(cmd_link(&store, "up rtt=3", Some(generation)), "OK\n");
        assert_eq!(fabric_state(), "connected");

        settle_work_owed(&store.as_store());
        backdate_work_owed(stale_after() * 100);
        assert_eq!(
            fabric_state(),
            "connected",
            "an idle bridge owes nothing: no work, no debt, no `stale`"
        );
        assert!(!wait_is_futile(), "a quiet link must still take a wait");

        assert_eq!(cmd_post(&ctx, "to=h-andrew kind=note one", None), "OK 1\n");
        assert_eq!(
            fabric_state(),
            "connected",
            "a fresh debt is not a stale one"
        );
        assert!(!wait_is_futile());

        backdate_work_owed(STALE_AFTER);
        assert_eq!(fabric_state(), "stale");
        assert!(wait_is_futile());
        let started = std::time::Instant::now();
        assert_eq!(
            cmd_post(&ctx, "to=h-andrew kind=ask --wait=600000 anyone?", None),
            "ERR fabric stale id=2 queued=1\n",
            "queued=1: the post IS in the outbox and the bridge drains it \
             the moment it moves again — `stale` is `wait, do not re-post`"
        );
        assert!(
            started.elapsed() < aterm_spec::wait::MUST_HAPPEN,
            "a stale wait is refused at once, not after its timeout: {:?}",
            started.elapsed()
        );

        assert_eq!(cmd_link(&store, "up rtt=4", Some(generation)), "OK\n");
        assert_eq!(fabric_state(), "connected");
        assert!(!wait_is_futile());

        assert_eq!(
            cmd_post(&ctx, "to=h-andrew kind=note three", None),
            "OK 3\n"
        );
        backdate_work_owed(stale_after());
        assert_eq!(fabric_state(), "stale");
        let next = next_bridge_generation();
        bridge_attached(next);
        assert_eq!(
            fabric_state(),
            "stalled",
            "an attach is `stalled`, not `stale`"
        );
        assert_eq!(cmd_link(&store, "up rtt=1", Some(next)), "OK\n");
        assert_eq!(
            fabric_state(),
            "connected",
            "the new incarnation has not been asked for anything yet"
        );
    });
}

#[test]
fn a_landing_re_arms_the_debt_while_another_post_is_still_outstanding() {
    with_link(|| {
        let store = new_store();
        let (sid, ctx) = registered(&store);
        let generation = next_bridge_generation();
        bridge_attached(generation);
        assert_eq!(cmd_link(&store, "up rtt=3", Some(generation)), "OK\n");
        assert_eq!(fabric_state(), "connected");

        assert_eq!(cmd_post(&ctx, "to=h-andrew kind=note one", None), "OK 1\n");
        assert_eq!(cmd_post(&ctx, "to=h-andrew kind=note two", None), "OK 2\n");

        assert_eq!(cmd_outbox_sent(&store, &format!("{sid} 1 off=7")), "OK\n");
        assert_eq!(
            fabric_state(),
            "connected",
            "a fresh re-stamp is not a stale one"
        );
        backdate_work_owed(stale_after());
        assert_eq!(
            fabric_state(),
            "stale",
            "post 2 is owed and the bridge has said nothing since"
        );
        assert!(wait_is_futile());

        assert_eq!(cmd_outbox_sent(&store, &format!("{sid} 2 off=8")), "OK\n");
        assert_eq!(fabric_state(), "connected");
        backdate_work_owed(stale_after() * 100);
        assert_eq!(
            fabric_state(),
            "connected",
            "an empty outbox owes nothing, however long ago the last ack was"
        );

        assert_eq!(
            cmd_post(&ctx, "to=h-andrew kind=note three", None),
            "OK 3\n"
        );
        backdate_work_owed(stale_after());
        assert_eq!(fabric_state(), "stale");
        assert_eq!(
            cmd_outbox_sent(&store, &format!("{sid} 3 off=- reason=undeliverable")),
            "OK\n"
        );
        assert_eq!(fabric_state(), "connected");
    });
}

#[test]
fn the_receipt_wait_wakes_at_the_stale_horizon_rather_than_at_its_bound() {
    with_link(|| {
        let store = new_store();
        let (sid, ctx) = registered(&store);
        let generation = next_bridge_generation();
        bridge_attached(generation);
        assert_eq!(cmd_link(&store, "up rtt=3", Some(generation)), "OK\n");

        assert_eq!(cmd_post(&ctx, "to=h-andrew kind=note one", None), "OK 1\n");
        assert_eq!(cmd_post(&ctx, "to=h-andrew kind=note two", None), "OK 2\n");
        assert_eq!(cmd_outbox_sent(&store, &format!("{sid} 1 off=7")), "OK\n");
        backdate_work_owed(stale_after() - std::time::Duration::from_millis(300));

        let started = std::time::Instant::now();
        assert_eq!(
            wait_receipt(&ctx, 1, 7, "", WAIT_MAX_MS),
            "ERR fabric stale id=1 off=7\n",
            "the post LANDED, so the reply names its offset and not `queued=1`"
        );
        assert!(
            started.elapsed() < aterm_spec::wait::MUST_HAPPEN,
            "the receipt wait sat out its bound instead of waking at the \
             stale horizon: {:?}",
            started.elapsed()
        );
    });
}

#[test]
fn a_parked_fetch_wakes_at_the_stale_horizon_rather_than_at_its_bound() {
    with_link(|| {
        let store = new_store();
        let (_sid, ctx) = registered(&store);
        attach_up(&store, next_bridge_generation());
        assert_eq!(
            inbox_get_at_within(&ctx, 7, std::time::Duration::from_millis(50)),
            "ERR timeout off=7\n",
            "nothing owed: no debt can go stale inside the park"
        );

        assert_eq!(cmd_post(&ctx, "to=h-andrew kind=note one", None), "OK 1\n");
        backdate_work_owed(stale_after() - std::time::Duration::from_millis(300));
        let started = std::time::Instant::now();
        assert_eq!(
            inbox_get_at_within(&ctx, 7, std::time::Duration::from_millis(WAIT_MAX_MS)),
            "ERR fabric stale off=7\n"
        );
        assert!(
            started.elapsed() < aterm_spec::wait::MUST_HAPPEN,
            "the fetch sat out its bound instead of waking at the stale \
             horizon: {:?}",
            started.elapsed()
        );
    });
}

#[test]
fn a_wait_parked_with_nothing_owed_answers_stale_when_a_later_debt_goes_stale() {
    use std::time::Duration;
    with_link(|| {
        // A fetch parked with nothing owed looks again a horizon later: 300 ms here, not the
        // instance's 6 s.
        STALE_AFTER_IN_TEST.store(300, Ordering::Relaxed);
        let store = new_store();
        let (_sid, ctx) = registered(&store);
        let (_peer_sid, peer) = registered_as(&store, 2);
        attach_up(&store, next_bridge_generation());
        assert_eq!(
            inbox_get_at_within(&ctx, 7, Duration::from_millis(50)),
            "ERR timeout off=7\n",
            "no debt armed: the read ends at its bound"
        );

        let watcher = test_support::Subscription::single(&ctx.watchers);
        let started = std::time::Instant::now();
        let reader = {
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                inbox_get_at_within(&ctx, 7, Duration::from_millis(WAIT_MAX_MS))
            })
        };
        assert!(
            watcher.wait(aterm_spec::wait::MUST_HAPPEN),
            "the parked fetch woke its watchers"
        );
        {
            let inbox = ctx.fabric.lock();
            assert!(
                inbox
                    .fetches
                    .iter()
                    .any(|(o, slot)| *o == 7 && matches!(slot, FetchSlot::Pending)),
                "the fetch released its guard, so it is parked"
            );
            assert_eq!(stale_in(), None, "and it parked with nothing owed");
        }

        assert_eq!(cmd_post(&peer, "to=h-andrew kind=note two", None), "OK 1\n");
        backdate_work_owed(stale_after());
        assert_eq!(fabric_state(), "stale");
        assert_eq!(
            reader.join().expect("the reader finished"),
            "ERR fabric stale off=7\n"
        );
        assert!(
            started.elapsed() < aterm_spec::wait::MUST_HAPPEN,
            "the fetch sat out its bound instead of looking again by the \
             horizon of the debt armed after it parked: {:?}",
            started.elapsed()
        );
    });
}

#[test]
fn a_ghost_lanes_link_report_is_dropped_and_a_lost_bridge_stays_lost() {
    with_link(|| {
        let store = new_store();
        let (_sid, _ctx) = registered(&store);
        let first = next_bridge_generation();
        attach_up(&store, first);
        let second = next_bridge_generation();
        bridge_attached(second);
        assert_eq!(
            fabric_state(),
            "stalled",
            "the replacement has not reported yet"
        );
        assert_eq!(
            cmd_link(&store, "up rtt=1", Some(first)),
            "OK stale=1\n",
            "a ghost's report is dropped"
        );
        assert_eq!(fabric_state(), "stalled");
        assert_eq!(cmd_link(&store, "up rtt=1", None), "OK stale=1\n");
        let (other_sid, _) = registered_as(&store, 2);
        assert_eq!(
            deliver(
                &store,
                &other_sid,
                1,
                "s-peer",
                "note",
                "unscoped%20fixture"
            ),
            "OK 1\n"
        );
        assert_eq!(
            fabric_state(),
            "stalled",
            "an unrelated unscoped delivery cannot supply this bridge's link evidence"
        );
        assert_eq!(cmd_link(&store, "up rtt=2", Some(second)), "OK\n");
        assert_eq!(fabric_state(), "connected");

        assert_eq!(
            bridge_lost(&store, second),
            1,
            "the delivered fixture session is governed"
        );
        assert_eq!(fabric_state(), "disconnected");
        assert_eq!(fabric_link_facts().0, LINK_BRIDGE_LOST);
        assert_eq!(
            cmd_link(&store, "up rtt=2", Some(second)),
            "OK stale=1\n",
            "a report after the guard fired cannot resurrect the link"
        );
        assert_eq!(fabric_state(), "disconnected");

        assert_eq!(cmd_link(&store, "up rtt=2", None), "OK stale=1\n");
    });
}

fn link_generation_admission_model() -> aterm_spec::derive::Model {
    aterm_spec::ty_model! {
        FabricLinkGenerationAdmission {
            const Buggy = 0;
            var owner = 2;
            var actor = 0;
            var lost = 0;
            var accepted = 0;
            action PickCurrent when (actor == 0 && accepted == 0) { actor = 2; }
            action PickStale when (actor == 0 && accepted == 0) { actor = 1; }
            action Lose when (lost == 0) { lost = 1; accepted = 0; }
            action Connect when (accepted == 0 && lost == 0 && owner > 0 &&
                (actor == owner || (Buggy == 1 && actor == 0))) { accepted = 1; }
            invariant ExplicitLiveOwnerOnly:
                accepted == 0 || (actor == owner && actor > 0 && lost == 0);
        }
    }
}

#[test]
fn link_reports_and_legacy_evidence_require_the_explicit_live_owner() {
    with_link(|| {
        let store = new_store();
        let model = link_generation_admission_model();
        for lost in [false, true] {
            for actor in 0..=2 {
                for report in [false, true] {
                    let stale = next_bridge_generation();
                    let current = next_bridge_generation();
                    bridge_attached(current);
                    if lost {
                        bridge_lost(&store, current);
                    }
                    let generation = match actor {
                        0 => None,
                        1 => Some(stale),
                        _ => Some(current),
                    };
                    let mut state = model.init_state();
                    state.insert("actor", actor);
                    state.insert("lost", i64::from(lost));
                    let admitted = model.action_enabled("Connect", &state);
                    if report {
                        assert_eq!(link_report(&store, generation, true, "", Some(2)), admitted);
                    } else {
                        link_evidence(generation);
                    }
                    assert_eq!(
                        fabric_state() == "connected",
                        admitted,
                        "report={report}, {state:?}"
                    );
                    if lost {
                        assert_eq!(fabric_state(), "disconnected");
                    }
                    if actor == 0 && !lost {
                        assert!(!generation.is_some_and(|g| g.0 != current.0));
                        state.insert("accepted", 1);
                        assert!(!model.check_invariant("ExplicitLiveOwnerOnly", &state));
                    }
                }
            }
        }
    });
}

#[test]
fn every_stored_link_transition_reaches_the_hook_once_in_order() {
    static HEARD: Mutex<Vec<LinkMove>> = Mutex::new(Vec::new());
    fn hear(m: LinkMove) {
        HEARD.lock().unwrap_or_else(|p| p.into_inner()).push(m);
    }
    on_link_moved(hear);
    with_link(|| {
        HEARD.lock().unwrap_or_else(|p| p.into_inner()).clear();
        let store = new_store();
        let g = next_bridge_generation();
        bridge_attached(g);
        assert_eq!(cmd_link(&store, "up rtt=3", Some(g)), "OK\n");
        assert_eq!(cmd_link(&store, "up rtt=4", Some(g)), "OK\n");
        assert_eq!(cmd_link(&store, "down reason=refused", Some(g)), "OK\n");
        assert_eq!(cmd_link(&store, "down reason=gone", Some(g)), "OK\n");
        let ghost = next_bridge_generation();
        assert!(!link_report(&store, Some(ghost), true, "", Some(1)));
        bridge_lost(&store, g);
        bridge_lost(&store, g);
        let quiet = next_bridge_generation();
        bridge_attached(quiet);
        link_evidence(Some(quiet));
        let heard: Vec<(&str, u64, LinkSaid)> = HEARD
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .filter(|m| [g.0, quiet.0].contains(&m.generation))
            .map(|m| (m.state, m.generation, m.said))
            .collect();
        assert_eq!(
            heard,
            [
                ("stalled", g.0, LinkSaid::Attached),
                ("connected", g.0, LinkSaid::Report),
                ("stalled", g.0, LinkSaid::Report),
                ("disconnected", g.0, LinkSaid::Lost),
                ("stalled", quiet.0, LinkSaid::Attached),
                ("connected", quiet.0, LinkSaid::Evidence),
            ]
        );
    });
}

#[test]
fn review_an_old_bridge_that_delivers_mail_reads_connected() {
    with_link(|| {
        let store = new_store();
        let (sid, ctx) = registered(&store);
        let generation = next_bridge_generation();
        bridge_attached(generation);
        bridge_attached(generation);
        assert_eq!(fabric_state(), "stalled");
        assert_eq!(fabric_link_facts().0, LINK_STARTING);
        assert_eq!(
            cmd_post(&ctx, "to=h-andrew kind=ask --wait=0 up?", None),
            "ERR timeout id=1\n",
            "under `starting` the wait parks and runs its course; it is not refused"
        );
        let delivered = on_lane(generation, || {
            deliver(&store, &sid, 3, "s-peer", "note", "hi")
        });
        assert!(delivered.starts_with("OK"), "{delivered}");
        assert_eq!(fabric_state(), "connected");
        let (reason, rtt, age) = fabric_link_facts();
        assert_eq!(
            (reason.as_str(), rtt),
            ("-", None),
            "no round trip was ever reported; the delivery is the ack"
        );
        assert!(
            age.is_some_and(
                |a| std::time::Duration::from_millis(a) < aterm_spec::wait::MUST_HAPPEN
            ),
            "{age:?}"
        );
        let tail = fabric_status_tail();
        assert!(
            tail.starts_with("fabric_rtt_ms=- fabric_link_age_ms=")
                && !tail.ends_with("fabric_link_age_ms=-"),
            "{tail}"
        );

        assert_eq!(
            cmd_link(&store, "down reason=closed", Some(generation)),
            "OK\n"
        );
        assert_eq!(fabric_state(), "stalled");
        let delivered = on_lane(generation, || {
            deliver(&store, &sid, 4, "s-peer", "note", "again")
        });
        assert!(delivered.starts_with("OK"), "{delivered}");
        assert_eq!(fabric_state(), "stalled");
        assert_eq!(
            cmd_post(&ctx, "to=h-andrew kind=ask --wait=0 up?", None),
            "ERR fabric stalled id=2 queued=1\n"
        );
        let next = next_bridge_generation();
        bridge_attached(next);
        assert_eq!(fabric_state(), "stalled");
        assert_eq!(fabric_link_facts().0, LINK_STARTING);
        let delivered = on_lane(next, || deliver(&store, &sid, 5, "s-peer", "note", "third"));
        assert!(delivered.starts_with("OK"), "{delivered}");
        assert_eq!(fabric_state(), "connected");
    });
}

#[test]
fn a_parked_waiter_is_woken_by_a_down_report_from_starting_or_up() {
    for was_up in [false, true] {
        with_link(|| {
            stale_after_a_hang();
            let store = new_store();
            let (_sid, ctx) = registered(&store);
            let generation = next_bridge_generation();
            if was_up {
                attach_up(&store, generation);
            } else {
                bridge_attached(generation);
                assert_eq!(fabric_link_facts().0, LINK_STARTING);
            }
            let poster = {
                let ctx = ctx.clone();
                std::thread::spawn(move || {
                    cmd_post(
                        &ctx,
                        "to=@s-peer kind=ask --wait=600000 which branch?",
                        None,
                    )
                })
            };
            let deadline = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
            while !cmd_inbox(&ctx, "--peek").contains("post 1 ") {
                assert!(
                    std::time::Instant::now() < deadline,
                    "the post never reached the outbox"
                );
                aterm_uds::deadline::poll_yield();
            }
            assert_eq!(
                cmd_link(&store, "down reason=no-socket", Some(generation)),
                "OK\n"
            );
            assert_eq!(
                poster.join().expect("the poster thread"),
                "ERR fabric stalled id=1 queued=1\n",
                "was_up={was_up}: a down report must wake a parked waiter"
            );
        });
    }
}

#[test]
fn a_lost_bridges_advisory_is_one_fact() {
    with_link(|| {
        let store = new_store();
        let (sid, ctx) = registered(&store);
        let generation = next_bridge_generation();
        bridge_attached(generation);
        assert_eq!(bridge_hold(&store, &format!("{sid} off")), "OK hold=0\n");
        assert_eq!(bridge_lost(&store, generation), 1);
        let facts = ctx.timeline.lock().unwrap().facts.clone();
        let advised: Vec<_> = facts
            .iter()
            .filter(|f| f.starts_with("hold-advisory: "))
            .collect();
        assert_eq!(
            advised,
            [&format!(
                "hold-advisory: Advisory {{ origin: \"fleet\", reason: \"fabric-lost\", \
                 generation: {} }}",
                generation.0
            )],
            "{facts:?}"
        );
    });
}

#[test]
fn a_session_the_bridge_only_unheld_is_still_governed() {
    with_link(|| {
        set_hold_on_bridge_lost(true);
        let store = new_store();
        let (sid, ctx) = registered(&store);
        let generation = next_bridge_generation();
        bridge_attached(generation);
        assert_eq!(bridge_hold(&store, &format!("{sid} off")), "OK hold=0\n");
        assert_eq!(bridge_lost(&store, generation), 1);
        assert!(halt_refusal(&ctx, "turn").is_some());
    });
}

#[test]
fn a_hold_naming_its_session_with_an_at_gets_the_bare_sid_form() {
    with_link(|| {
        let store = new_store();
        let (sid, ctx) = registered(&store);
        let bare = format!(
            "ERR usage: hold names its session by its bare sid, with no @: {}",
            HOLD_USAGE.trim_start_matches("ERR usage: ")
        );
        for issuer in [HoldIssuer::Owner, HoldIssuer::Bridge] {
            for rest in [
                format!("@{sid} on"),
                format!("@{sid} off reason=x"),
                "@s-nope on".to_owned(),
                "@self on".to_owned(),
            ] {
                assert_eq!(cmd_hold(&store, &rest, issuer), bare, "{issuer:?} {rest}");
            }
        }
        assert_eq!(ctx.fabric.hold(), None, "nothing was held");
        assert_eq!(owner_hold(&store, &format!("{sid} on")), "OK hold=1\n");
    });
}

#[test]
fn a_bridge_hold_over_a_local_one_stands_against_the_owner() {
    with_link(|| {
        let store = new_store();
        let (sid, ctx) = registered(&store);
        let hold = |reason: &str, origin: &str| Hold {
            reason: reason.to_string(),
            origin: origin.to_string(),
        };
        let standing = || ctx.fabric.holds().standing().collect::<Vec<_>>();
        assert_eq!(owner_hold(&store, &format!("{sid} off")), "OK hold=0\n");
        assert_eq!(
            owner_hold(&store, &format!("{sid} on reason=mine origin=local")),
            "OK hold=1\n"
        );

        let local = vec![hold("mine", "local")];
        let serial = ctx.fabric.hold_serial();
        for lift in [format!("{sid} off"), format!("{sid} off origin=fleet")] {
            assert_eq!(bridge_hold(&store, &lift), "OK hold=1\n", "{lift}");
            assert_eq!(standing(), local, "{lift}");
        }
        assert_eq!(ctx.fabric.hold_serial(), serial, "nothing moved");
        assert_eq!(
            halt_refusal(&ctx, "turn").as_deref(),
            Some("ERR halted reason=mine origin=local\n")
        );
        assert_eq!(
            bridge_hold(&store, &format!("{sid} off origin=local")),
            "OK hold=0\n"
        );
        assert!(halt_refusal(&ctx, "turn").is_none());
        assert_eq!(
            bridge_hold(&store, &format!("{sid} on reason=stop")),
            "OK hold=1\n"
        );
        assert_eq!(
            bridge_hold(&store, &format!("{sid} off origin=local")),
            "OK hold=1\n"
        );
        assert_eq!(
            halt_refusal(&ctx, "turn").as_deref(),
            Some("ERR halted reason=stop origin=fleet\n")
        );
        assert_eq!(bridge_hold(&store, &format!("{sid} off")), "OK hold=0\n");

        assert_eq!(
            owner_hold(&store, &format!("{sid} on reason=mine")),
            "OK hold=1\n"
        );
        assert_eq!(
            bridge_hold(&store, &format!("{sid} on reason=fabric-lost")),
            "OK hold=1\n"
        );
        assert_eq!(
            standing(),
            [hold("fabric-lost", "fleet"), hold("mine", "local")]
        );
        assert_eq!(owner_hold(&store, &format!("{sid} off")), "OK hold=1\n");
        assert_eq!(
            halt_refusal(&ctx, "turn").as_deref(),
            Some("ERR halted reason=fabric-lost origin=fleet\n")
        );
    });
}

fn single_slot(standing: Option<Hold>, act: &HoldAct<'_>) -> Option<Hold> {
    match act {
        HoldAct::Set(hold) | HoldAct::Lost(hold, _) => Some(hold.clone()),
        HoldAct::Lift(origin) => standing.filter(|hold| hold.origin != *origin),
        HoldAct::LiftAll => None,
        HoldAct::SetRemote(_) | HoldAct::LiftRemote(_) => standing,
    }
}

#[test]
fn a_fleet_lift_leaves_the_owners_stop_standing() {
    with_link(|| {
        let store = new_store();
        let (sid, ctx) = registered(&store);
        let mine = Hold {
            reason: "mine".to_string(),
            origin: "local".to_string(),
        };
        let stop = Hold {
            reason: "stop".to_string(),
            origin: "fleet".to_string(),
        };
        let acts = [
            HoldAct::Set(mine.clone()),
            HoldAct::Set(stop.clone()),
            HoldAct::Lift("fleet"),
        ];
        assert_eq!(
            acts.iter().fold(None, single_slot),
            None,
            "NEGATIVE CONTROL: one slot loses the owner's stop to this sequence"
        );

        assert_eq!(
            owner_hold(&store, &format!("{sid} on reason=mine")),
            "OK hold=1\n"
        );
        assert_eq!(
            bridge_hold(&store, &format!("{sid} on reason=stop")),
            "OK hold=1\n"
        );
        assert_eq!(
            ctx.fabric.holds().standing().collect::<Vec<_>>(),
            [stop, mine.clone()],
            "both stand"
        );
        assert_eq!(
            ctx.fabric.holds().origins(),
            "fleet,local",
            "`hold_origin=` names both"
        );
        assert_eq!(
            bridge_hold(&store, &format!("{sid} off")),
            "OK hold=1\n",
            "the fleet's lift answers that the session is still held"
        );

        assert_eq!(ctx.fabric.hold(), Some(mine), "the owner's stop stands");
        assert_eq!(ctx.fabric.holds().origins(), "local");
        assert_eq!(
            halt_refusal(&ctx, "send").as_deref(),
            Some("ERR halted reason=mine origin=local\n")
        );
        assert!(cmd_inbox(&ctx, "--peek").starts_with("OK 0 hold=1 "));
        let events: Vec<String> = ctx
            .timeline
            .lock()
            .unwrap()
            .since(None)
            .filter(|e| e.kind == "hold")
            .map(|e| e.fact.clone())
            .collect();
        assert_eq!(
            events,
            [
                "1 reason=mine origin=local hold_origin=local",
                "1 reason=stop origin=fleet hold_origin=fleet,local",
                "1 reason=mine origin=local hold_origin=local",
            ],
            "every event says what stands after it"
        );
        assert_eq!(owner_hold(&store, &format!("{sid} off")), "OK hold=0\n");
        assert!(halt_refusal(&ctx, "send").is_none());
        let last = ctx
            .timeline
            .lock()
            .unwrap()
            .since(None)
            .rfind(|e| e.kind == "hold")
            .map(|e| e.fact.clone());
        assert_eq!(
            last.as_deref(),
            Some("0 reason=- origin=local hold_origin=-")
        );
    });
}

#[test]
fn a_reconnect_after_the_sweep_leaves_the_owners_stop_standing() {
    with_link(|| {
        set_hold_on_bridge_lost(true);
        let store = new_store();
        let (sid, ctx) = registered(&store);
        let other = test_handle(2);
        let (other_sid, other_ctx) = (other.sid.as_str().to_string(), other.ctx.clone());
        store
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .register(other);
        let generation = next_bridge_generation();
        bridge_attached(generation);
        for s in [&sid, &other_sid] {
            assert_eq!(bridge_hold(&store, &format!("{s} off")), "OK hold=0\n");
        }
        assert_eq!(
            owner_hold(&store, &format!("{sid} on reason=mine")),
            "OK hold=1\n"
        );

        assert_eq!(bridge_lost(&store, generation), 2, "two governed sessions");
        assert_eq!(ctx.fabric.holds().origins(), "fleet,local");
        assert_eq!(
            halt_refusal(&ctx, "send").as_deref(),
            Some("ERR halted reason=fabric-lost origin=fleet\n")
        );
        assert_eq!(other_ctx.fabric.holds().origins(), "fleet");

        let next = next_bridge_generation();
        bridge_attached(next);
        assert_eq!(bridge_hold(&store, &format!("{sid} off")), "OK hold=1\n");
        assert_eq!(
            bridge_hold(&store, &format!("{other_sid} off")),
            "OK hold=0\n"
        );
        assert_eq!(
            halt_refusal(&ctx, "send").as_deref(),
            Some("ERR halted reason=mine origin=local\n"),
            "the owner's stop survived the sweep and its lift"
        );
        assert!(
            halt_refusal(&other_ctx, "send").is_none(),
            "NEGATIVE CONTROL: the same lift frees a session with no hold of the owner's"
        );
    });
}

#[test]
fn await_inbox_latches_forward_only_and_ignores_notes_by_default() {
    let store = new_store();
    let (sid, ctx) = registered(&store);
    assert_eq!(deliver(&store, &sid, 1, "h-a", "task", "old"), "OK 1\n");
    assert_eq!(cmd_await_inbox(&ctx, &["since=1"], 5), "OK timeout\n");
    assert_eq!(deliver(&store, &sid, 2, "h-a", "note", "fyi"), "OK 2\n");
    assert_eq!(
        cmd_await_inbox(&ctx, &["since=1"], 5),
        "OK timeout\n",
        "a note does not wake an agent by itself"
    );
    assert_eq!(
        cmd_await_inbox(&ctx, &["since=1", "kinds=note"], 5),
        "OK inbox 2\n",
        "unless the caller asked for notes"
    );
    assert_eq!(deliver(&store, &sid, 3, "h-a", "ask", "?"), "OK 3\n");
    assert_eq!(cmd_await_inbox(&ctx, &["since=1"], 5), "OK inbox 3\n");
    assert_eq!(
        cmd_await_inbox(&ctx, &["since=1"], u64::MAX),
        "OK inbox 3\n",
        "extreme timeout_ms saturates cleanly without Instant overflow"
    );
    assert_eq!(
        cmd_await_inbox(&ctx, &["since=3"], 5),
        "OK timeout\n",
        "monotone: the row it already latched on cannot latch it again"
    );
    assert_eq!(
        cmd_await_inbox(&ctx, &["since=1", "kinds=answer"], 5),
        "OK timeout\n"
    );
    assert_eq!(
        bridge_hold(&store, &format!("{sid} on reason=x")),
        "OK hold=1\n"
    );
    assert_eq!(
        cmd_await_inbox(&ctx, &["since=3", "kinds=hold"], 5),
        "OK timeout\n",
        "a halt already in force is not news"
    );
    let waiter = {
        let ctx = ctx.clone();
        std::thread::spawn(move || cmd_await_inbox(&ctx, &["since=3", "kinds=hold"], 60_000))
    };
    let mut on = true;
    while !waiter.is_finished() {
        let arg = if on { "on reason=x" } else { "off" };
        bridge_hold(&store, &format!("{sid} {arg}"));
        on = !on;
        aterm_uds::deadline::poll_yield();
    }
    let latched = waiter.join().expect("the waiter finished");
    assert!(
        latched == "OK inbox hold=1\n" || latched == "OK inbox hold=0\n",
        "a hold TRANSITION latches the wait, not the timeout: {latched}"
    );
    bridge_hold(&store, &format!("{sid} off"));
    assert_eq!(
        cmd_await_inbox(&ctx, &["since=3", "kinds=task"], 5),
        "OK timeout\n"
    );
    assert!(cmd_await_inbox(&ctx, &[], 5).starts_with("ERR usage"));
    assert!(cmd_await_inbox(&ctx, &["since=1", "kinds=nope"], 5).starts_with("ERR usage"));
}

#[test]
fn await_inbox_topic_latches_only_on_that_topics_broadcasts() {
    let store = new_store();
    let (sid, ctx) = registered(&store);
    for topic in ["news", "lab"] {
        assert_eq!(ctx.fabric.topic_add(topic, "head", &ctx.timeline), Ok(true));
    }
    let shout = |off: u64, topic: &str| {
        cmd_deliver(
            &store,
            &format!(
                "{sid} off={off} from=s-peer@n-lab kind=note trust=agent topic={topic} \
                 text=shout"
            ),
        )
    };
    assert_eq!(deliver(&store, &sid, 1, "h-a", "task", "go"), "OK 1\n");
    assert_eq!(shout(2, "news"), "OK 2\n");
    assert_eq!(shout(3, "lab"), "OK 3\n");
    assert_eq!(
        cmd_await_inbox(&ctx, &["since=0", "topic=lab"], 5),
        "OK inbox 3\n",
        "a note on the topic latches with no kinds="
    );
    assert_eq!(
        cmd_await_inbox(&ctx, &["since=0", "topic=news"], 5),
        "OK inbox 2\n",
        "the newest row on THAT topic, not the newest row"
    );
    assert_eq!(
        cmd_await_inbox(&ctx, &["since=0", "topic=lab", "kinds=task,ask"], 5),
        "OK timeout\n",
        "kinds= still narrows"
    );
    assert_eq!(
        cmd_await_inbox(&ctx, &["since=0", "topic=lab", "kinds=note"], 5),
        "OK inbox 3\n"
    );
    assert_eq!(deliver(&store, &sid, 4, "h-a", "task", "more"), "OK 4\n");
    assert_eq!(deliver(&store, &sid, 5, "h-a", "note", "fyi"), "OK 5\n");
    assert_eq!(shout(6, "news"), "OK 6\n");
    assert_eq!(
        cmd_await_inbox(&ctx, &["since=3", "topic=lab"], 5),
        "OK timeout\n",
        "an addressed row and another topic's broadcast do not latch it"
    );
    assert_eq!(cmd_await_inbox(&ctx, &["since=3"], 5), "OK inbox 4\n");
    for bad in ["topic=", "topic=Lab", "topic=a/b", "topic=-x"] {
        assert!(
            cmd_await_inbox(&ctx, &["since=0", bad], 5).starts_with("ERR usage"),
            "{bad}"
        );
    }
    assert!(
        cmd_await_inbox(&ctx, &["topic=lab"], 5).starts_with("ERR usage"),
        "a topic is not a one-off: since= is still required"
    );
}

#[test]
fn an_accepted_post_wakes_before_its_wait_parks_and_a_refused_one_never() {
    use std::time::Duration;
    with_link(|| {
        stale_after_a_hang();
        let store = new_store();
        let (sid, ctx) = registered(&store);
        attach_up(&store, next_bridge_generation());
        let watcher = test_support::Subscription::single(&ctx.watchers);
        assert!(cmd_post(&ctx, "to=@s-b kind=shout hi", None).starts_with("ERR usage"));
        assert!(!watcher.wait(Duration::ZERO), "a refused post wakes nobody");
        assert_eq!(cmd_post(&ctx, "to=h-andrew kind=note hi", None), "OK 1\n");
        assert!(watcher.wait(Duration::ZERO), "an accepted post wakes");

        let poster = {
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                cmd_post(
                    &ctx,
                    &format!("to=h-andrew kind=task --wait={} ship it", hang_ms()),
                    None,
                )
            })
        };
        assert!(
            watcher.wait(stale_after() / 2),
            "the wake arrives while the post's wait is parked"
        );
        assert!(
            !poster.is_finished(),
            "nothing has landed, so the post is still parked in its wait"
        );
        assert_eq!(
            cmd_deliver(&store, &format!("{sid} landed=2 off=42")),
            "OK\n"
        );
        assert_eq!(poster.join().expect("the poster finished"), "OK 2 off=42\n");
    });
}

#[test]
fn inbox_seen_reports_the_addressed_floor_and_no_broadcast_moves_it() {
    let store = new_store();
    let (sid, ctx) = registered(&store);
    assert_eq!(ctx.fabric.topic_add("t", "head", &ctx.timeline), Ok(true));
    let last_seen = || {
        ctx.timeline
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .since(None)
            .rfind(|e| e.kind == "inbox-seen")
            .map(|e| e.fact.clone())
            .unwrap_or_default()
    };
    assert_eq!(broadcast(&store, &sid, 1000, "t", "b0"), "OK 1\n");
    assert_eq!(cmd_inbox_seen(&ctx, "1"), "OK seen=1\n");
    assert_eq!(
        last_seen(),
        "1 off=1000 floor=- seen=1",
        "no addressed row yet: no floor to say"
    );

    assert_eq!(broadcast(&store, &sid, 1100, "t", "b1"), "OK 2\n");
    assert_eq!(deliver(&store, &sid, 900, "h-a", "task", "a1"), "OK 3\n");
    assert_eq!(cmd_inbox_seen(&ctx, "2"), "OK seen=2\n");
    assert_eq!(
        last_seen(),
        "2 off=1100 floor=900 seen=2",
        "the unhandled task holds the floor below the broadcast's offset"
    );
    assert_eq!(cmd_inbox_seen(&ctx, "3 handled"), "OK seen=3\n");
    assert_eq!(
        last_seen(),
        "3 off=900 floor=901 seen=3 verdict=handled kind=task from=h-a"
    );

    assert_eq!(deliver(&store, &sid, 2000, "h-a", "task", "a2"), "OK 4\n");
    assert_eq!(broadcast(&store, &sid, 1200, "t", "b2"), "OK 5\n");
    assert_eq!(cmd_inbox_seen(&ctx, "5"), "OK seen=5\n");
    assert_eq!(
        last_seen(),
        "5 off=1200 floor=2001 seen=5",
        "the handled task at 2000 is behind the floor, whatever the broadcast's offset"
    );
}

#[test]
fn deliver_wakes_a_mail_watch() {
    use std::time::Duration;
    let store = new_store();
    let (sid, ctx) = registered(&store);
    let watcher = test_support::Subscription::single(&ctx.watchers);
    assert_eq!(
        deliver(&store, &sid, 10, "h-andrew", "task", "one"),
        "OK 1\n"
    );
    assert!(watcher.wait(Duration::ZERO), "the delivery woke the watch");
    assert_eq!(
        deliver(&store, &sid, 10, "h-andrew", "task", "one"),
        "OK 1\n"
    );
    assert!(!watcher.wait(Duration::ZERO), "a redelivery wakes nobody");
}

#[test]
fn inbox_seen_wakes_its_watchers_with_its_row_and_a_refused_form_never() {
    use std::time::Duration;
    let store = new_store();
    let (sid, ctx) = registered(&store);
    assert_eq!(
        deliver(&store, &sid, 10, "h-andrew", "task", "one"),
        "OK 1\n"
    );
    let watcher = test_support::Subscription::single(&ctx.watchers);
    let last_kind = || {
        ctx.timeline
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .since(None)
            .next_back()
            .map(|e| e.kind)
    };
    for (form, reply) in [
        ("seen 9", "ERR no such message\n".to_string()),
        ("seen x", INBOX_USAGE.to_string()),
        ("seen 1 later", INBOX_USAGE.to_string()),
        ("get 1", "OK 3\none".to_string()),
        ("--peek", cmd_inbox(&ctx, "--peek")),
    ] {
        assert_eq!(cmd_inbox_verb(&ctx, form), reply, "{form}");
    }
    assert!(
        !watcher.wait(Duration::ZERO),
        "nothing recorded, nobody woken"
    );
    assert_eq!(cmd_inbox_verb(&ctx, "seen 1"), "OK seen=1\n");
    assert!(
        watcher.wait(Duration::ZERO),
        "a bare seen moves the floor: woken"
    );
    assert_eq!(last_kind(), Some("inbox-seen"), "by the row it announces");
    assert_eq!(cmd_inbox_verb(&ctx, "seen 1 handled"), "OK seen=1\n");
    assert!(
        watcher.wait(Duration::ZERO),
        "a verdict owes a receipt: woken"
    );
}

#[test]
fn a_parked_fetch_wakes_its_watchers_before_it_parks_and_a_ring_read_never() {
    use std::time::Duration;
    with_link(|| {
        let store = new_store();
        let (sid, ctx) = registered(&store);
        attach_up(&store, next_bridge_generation());
        assert_eq!(
            deliver(&store, &sid, 10, "h-andrew", "task", "one"),
            "OK 1\n"
        );
        let watcher = test_support::Subscription::single(&ctx.watchers);
        assert_eq!(
            cmd_inbox_verb(&ctx, "get @10"),
            "OK 3 off=10 from=h-andrew kind=task trust=agent\none"
        );
        assert!(!watcher.wait(Duration::ZERO), "a ring read woke the bridge");

        let reader = {
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                fetch_waits_a_hang();
                cmd_inbox_verb(&ctx, "get @7")
            })
        };
        assert!(
            watcher.wait(aterm_spec::wait::MUST_HAPPEN),
            "a parked fetch woke its watchers"
        );
        assert!(
            cmd_outbox(&store, "").contains(&format!("fetch sid={sid} off=7\n")),
            "woken with the fetch on the outbox peek"
        );
        assert_eq!(
            cmd_deliver(&store, &format!("{sid} fetched=7 err=no-record")),
            "OK\n"
        );
        assert_eq!(
            reader.join().expect("the reader finished"),
            "ERR no such record off=7\n",
            "the wake came before the park, so the park took the answer"
        );
    });
}

#[test]
fn post_key_is_bounded_rides_the_outbox_and_a_dup_retirement_reaches_the_wait() {
    with_link(|| {
        stale_after_a_hang();
        let store = new_store();
        let (sid, ctx) = registered(&store);
        assert!(valid_key("k-1") && valid_key("0123:abcd:99") && valid_key("a.b_c"));
        assert!(valid_key(&"k".repeat(KEY_MAX)));
        assert!(!valid_key(""));
        assert!(!valid_key(&"k".repeat(KEY_MAX + 1)));
        assert!(!valid_key("k/1") && !valid_key("k@1") && !valid_key("k=1"));
        assert!(!valid_key("kä"), "not ASCII");
        for bad in ["key= x", "key=k/1 x", "key=a@b x"] {
            assert_eq!(
                cmd_post(&ctx, &format!("to=@s-b kind=note {bad}"), None),
                POST_USAGE,
                "{bad}"
            );
        }
        assert_eq!(
            cmd_post(
                &ctx,
                &format!("to=@s-b kind=note key={} x", "k".repeat(KEY_MAX + 1)),
                None
            ),
            POST_USAGE
        );

        assert_eq!(
            cmd_post(&ctx, "to=@s-b kind=note key=k-1 hi", None),
            "OK 1\n"
        );
        let (_, posts) = parse_outbox(&cmd_outbox(&store, ""));
        assert_eq!(posts.len(), 1);
        assert!(
            posts[0].0.ends_with(" key=k-1 len=2"),
            "the key rides the outbox line, before len=: {:?}",
            posts[0].0
        );
        let (_, again) = parse_outbox(&cmd_outbox(&store, ""));
        assert_eq!(
            again, posts,
            "a peek: the key is there on the next drain too"
        );
        assert!(
            cmd_inbox(&ctx, "--peek").contains("post 1 to=@s-b kind=note off=- len=2 key=k-1"),
            "{}",
            cmd_inbox(&ctx, "--peek")
        );
        assert_eq!(cmd_post(&ctx, "to=@s-b kind=note plain", None), "OK 2\n");
        let (_, both) = parse_outbox(&cmd_outbox(&store, ""));
        assert!(!both[1].0.contains("key="), "{:?}", both[1].0);

        assert_eq!(
            cmd_outbox_sent(&store, &format!("{sid} 1 off=- dup=1")),
            OUTBOX_USAGE
        );
        assert_eq!(
            cmd_outbox_sent(&store, &format!("{sid} 1 off=42 dup=1")),
            "OK\n"
        );
        {
            let inbox = ctx.fabric.lock();
            let row = inbox.posts.iter().find(|p| p.id == 1).expect("post 1 kept");
            assert_eq!((row.off, row.dup), (Some(42), true));
            assert_eq!(row.key, None, "the key goes with the body at retirement");
        }
        assert_eq!(cmd_outbox_sent(&store, &format!("{sid} 1 off=42")), "OK\n");
        assert!(ctx.fabric.lock().posts.iter().any(|p| p.id == 1 && p.dup));

        attach_up(&store, next_bridge_generation());
        let poster = {
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                cmd_post(
                    &ctx,
                    &format!("to=h-andrew kind=task key=k-2 --wait={} ship it", hang_ms()),
                    None,
                )
            })
        };
        loop {
            let reply = cmd_outbox_sent(&store, &format!("{sid} 3 off=42 dup=1"));
            if reply == "OK\n" {
                break;
            }
            assert_eq!(reply, "ERR no such post\n");
            aterm_uds::deadline::poll_yield();
        }
        assert_eq!(
            poster.join().expect("the poster finished"),
            "OK 3 off=42 dup=1\n",
            "the re-post answers the ORIGINAL offset and says it was a duplicate"
        );
    });
}

#[test]
fn post_refuses_what_it_cannot_address() {
    let store = new_store();
    let (_sid, ctx) = registered(&store);
    assert!(cmd_post(&ctx, "to=@s-b kind=shout hi", None).starts_with("ERR usage"));
    assert!(cmd_post(&ctx, "kind=note hi", None).starts_with("ERR usage"));
    assert!(cmd_post(&ctx, "to=@s-b kind=note", None).starts_with("ERR usage"));
    assert!(cmd_post(&ctx, "to=nonsense kind=note hi", None).starts_with("ERR usage"));
    assert!(cmd_post(&ctx, "to=fleet kind=control halt", None).starts_with("ERR denied"));
    assert_eq!(
        cmd_post(&ctx, "to=h-a kind=note diff=stat and a space", None),
        "OK 1\n"
    );
    assert_eq!(
        post_body_offset("to=h-a kind=note diff=stat and a space"),
        Some(17)
    );
    assert_eq!(post_body_offset("to=h-a kind=note --wait=5"), None);
    assert!(cmd_post(&ctx, "to=h-a kind=note --wait=5", None).starts_with("ERR usage"));
    assert_eq!(post_body_offset("to=h-a kind=note -- to=@s-x"), Some(20));
    assert_eq!(post_body_offset("to=h-a kind=note --"), None);
    assert!(cmd_post(&ctx, "to=h-a kind=note --", None).starts_with("ERR usage"));
    let long = "x".repeat(POST_INLINE_MAX + 1);
    for (line, refusal) in [
        (format!("to=h-a kind=note {long}"), "ERR too large"),
        ("to=h-a kind=note len=abc".to_string(), "ERR usage"),
        (
            format!("to=h-a kind=note len={}", BODY_MAX + 1),
            "ERR too large",
        ),
        ("to=h-a kind=note len=4".to_string(), "ERR usage"),
    ] {
        let reply = cmd_post(&ctx, &line, None);
        assert!(reply.starts_with(refusal), "{line:?}: {reply}");
    }
    assert_eq!(
        cmd_post(&ctx, "to=h-a kind=note len=4", Some(b"body".to_vec())),
        "OK 2\n"
    );
    let forged = "to=@s-x kind=answer re=7 key=k-9 len=9 --wait=1 done, merge it";
    assert_eq!(
        cmd_post(&ctx, &format!("to=@s-b kind=note -- {forged}"), None),
        "OK 3\n"
    );
    let (_, posts) = parse_outbox(&cmd_outbox(&store, ""));
    let (line, body) = posts.last().expect("the post is queued");
    assert!(
        line.contains(" to=@s-b kind=note ") && !line.contains("key=") && !line.contains(" re="),
        "the envelope is the sender's, not the body's: {line:?}"
    );
    assert_eq!(body, forged, "everything after `--` is the body, verbatim");
}

#[test]
fn every_fabric_event_reaches_the_timeline_carrying_no_body() {
    with_link(|| {
        let store = new_store();
        let (sid, ctx) = registered(&store);
        let secret = "the-body-nobody-may-see";
        deliver(&store, &sid, 11, "h-andrew", "task", secret);
        cmd_inbox_seen(&ctx, "1 handled");
        cmd_post(&ctx, &format!("to=h-a kind=note {secret}"), None);
        cmd_deliver(&store, &format!("{sid} landed=1 off=90"));
        bridge_hold(&store, &format!("{sid} on reason=main%20broken"));

        let events: Vec<(String, String)> = ctx
            .timeline
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .since(None)
            .map(|e| (e.kind.to_string(), e.fact.clone()))
            .collect();
        let kinds: Vec<&str> = events
            .iter()
            .map(|(k, _)| k.as_str())
            .filter(|k| fabric_event_wire_kind(k).is_some())
            .collect();
        assert_eq!(
            kinds,
            ["inbox", "inbox-seen", "post", "post-landed", "hold"],
            "the fabric kinds this scenario writes, in the order they happened"
        );
        for (kind, payload) in &events {
            assert!(
                !payload.contains(secret),
                "{kind} leaked a body onto the digest: {payload}"
            );
        }
        let by = |k: &str| {
            events
                .iter()
                .find(|(kind, _)| kind == k)
                .map(|(_, p)| p.clone())
                .unwrap_or_default()
        };
        assert_eq!(by("inbox"), "1 from=h-andrew kind=task off=11");
        assert_eq!(
            by("inbox-seen"),
            "1 off=11 floor=12 seen=1 verdict=handled kind=task from=h-andrew"
        );
        assert_eq!(by("post"), "1 to=h-a kind=note");
        assert_eq!(by("post-landed"), "1 off=90");
        assert_eq!(
            by("hold"),
            "1 reason=main%20broken origin=fleet hold_origin=fleet"
        );
    });
}

#[test]
fn inbox_seen_records_a_verdict_once_per_change_and_the_bare_form_records_none() {
    with_link(|| {
        let store = new_store();
        let (sid, ctx) = registered(&store);
        deliver(&store, &sid, 10, "h-andrew", "task", "one");
        deliver(&store, &sid, 11, "s-peer@n-lab", "ask", "two");
        deliver(&store, &sid, 12, "h-andrew", "note", "three");
        cmd_inbox(&ctx, "--peek");
        assert_eq!(cmd_inbox_seen(&ctx, "2"), "OK seen=2\n");
        assert_eq!(cmd_inbox_seen(&ctx, "3 handled"), "OK seen=3\n");
        assert_eq!(cmd_inbox_seen(&ctx, "3 handled"), "OK seen=3\n");
        assert_eq!(cmd_inbox_seen(&ctx, "2 deferred"), "OK seen=3\n");
        assert_eq!(cmd_inbox_seen(&ctx, "2 handled"), "OK seen=3\n");
        assert_eq!(cmd_inbox_seen(&ctx, "2 later"), INBOX_USAGE);
        let seen: Vec<String> = ctx
            .timeline
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .since(None)
            .filter(|e| e.kind == "inbox-seen")
            .map(|e| e.fact.clone())
            .collect();
        assert_eq!(
            seen,
            [
                "2 off=11 floor=12 seen=2",
                "3 off=12 floor=13 seen=3 verdict=handled kind=note from=h-andrew",
                "3 off=12 floor=13 seen=3",
                "2 off=11 floor=13 seen=3 verdict=deferred kind=ask from=s-peer@n-lab",
                "2 off=11 floor=13 seen=3 verdict=handled kind=ask from=s-peer@n-lab",
            ],
            "one verdict per row per CHANGE; the bare form and a repeat carry none"
        );
    });
}

#[test]
fn an_ack_row_carries_its_verdict_and_await_inbox_re_latches_on_it() {
    with_link(|| {
        let store = new_store();
        let (sid, ctx) = registered(&store);
        assert_eq!(cmd_post(&ctx, "to=@s-peer kind=note do it", None), "OK 1\n");
        assert_eq!(
            cmd_deliver(&store, &format!("{sid} landed=1 off=70")),
            "OK\n"
        );
        assert_eq!(
            cmd_deliver(
                &store,
                &format!("{sid} off=80 from=s-peer@n-lab kind=ack re=70 verdict=maybe text=x")
            ),
            DELIVER_USAGE
        );
        let waiter = {
            let ctx = ctx.clone();
            std::thread::spawn(move || cmd_await_inbox(&ctx, &["re=70"], 60_000))
        };
        let other = {
            let ctx = ctx.clone();
            std::thread::spawn(move || cmd_await_inbox(&ctx, &["re=71", "since=0"], 200))
        };
        assert_eq!(
            cmd_deliver(
                &store,
                &format!(
                    "{sid} off=81 from=s-peer@n-lab kind=ack re=70 verdict=refused \
                     text=ack%20re%3D70%20verdict%3Drefused"
                )
            ),
            "OK 1\n"
        );
        assert_eq!(waiter.join().expect("the waiter finished"), "OK inbox 1\n");
        assert_eq!(other.join().expect("the other finished"), "OK timeout\n");
        let listing = cmd_inbox(&ctx, "--peek --meta");
        let row = listing.lines().nth(1).expect("the ack row");
        assert!(
            row.starts_with("msg 1 off=81 ")
                && row.contains(" kind=ack trust=agent re=70 re-id=1 verdict=refused "),
            "{row}"
        );
        assert_eq!(
            cmd_await_inbox(&ctx, &["re=70", "since=1"], 100),
            "OK timeout\n"
        );
        assert!(cmd_await_inbox(&ctx, &["kinds=ack"], 100).starts_with("ERR usage"));
    });
}

#[test]
fn post_wait_ack_returns_the_verdict_or_the_expiry_and_is_bounded_by_dl() {
    with_link(|| {
        stale_after_a_hang();
        let store = new_store();
        let (sid, ctx) = registered(&store);
        attach_up(&store, next_bridge_generation());
        assert_eq!(
            cmd_post(&ctx, "to=@s-peer kind=note --wait-ack hello", None),
            POST_USAGE
        );
        let poster = {
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                cmd_post(
                    &ctx,
                    &format!("to=@s-peer kind=task --wait-ack={} ship it", hang_ms()),
                    None,
                )
            })
        };
        loop {
            if cmd_deliver(&store, &format!("{sid} landed=1 off=42")) == "OK\n" {
                break;
            }
            aterm_uds::deadline::poll_yield();
        }
        assert_eq!(
            cmd_deliver(
                &store,
                &format!("{sid} off=50 from=s-peer@n-lab kind=ack re=42 verdict=handled text=ok")
            ),
            "OK 1\n"
        );
        assert_eq!(
            poster.join().expect("the poster finished"),
            "OK 1 off=42 ack=handled msg=1\n"
        );
        let poster = {
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                TEST_EXPIRY_GRACE_MS.with(|grace| grace.set(Some(hang_ms())));
                cmd_post(&ctx, "to=@s-peer kind=ask dl=30000 --wait-ack why?", None)
            })
        };
        loop {
            if cmd_deliver(&store, &format!("{sid} landed=2 off=43")) == "OK\n" {
                break;
            }
            aterm_uds::deadline::poll_yield();
        }
        assert_eq!(
            cmd_deliver(
                &store,
                &format!("{sid} off=51 from=n-lab kind=expired re=43 dl=30000 text=expired")
            ),
            "OK 2\n"
        );
        assert_eq!(
            poster.join().expect("the poster finished"),
            "ERR expired id=2 off=43\n"
        );
        const TEST_GRACE_MS: u64 = 1;
        let poster = {
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                TEST_EXPIRY_GRACE_MS.with(|grace| grace.set(Some(TEST_GRACE_MS)));
                cmd_post(&ctx, "to=@s-peer kind=task dl=150 --wait-ack quick", None)
            })
        };
        loop {
            if cmd_deliver(&store, &format!("{sid} landed=3 off=44")) == "OK\n" {
                break;
            }
            aterm_uds::deadline::poll_yield();
        }
        assert_eq!(
            poster.join().expect("the poster finished"),
            "ERR timeout id=3 off=44\n"
        );
        let poster = {
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                TEST_EXPIRY_GRACE_MS.with(|grace| grace.set(Some(hang_ms())));
                cmd_post(&ctx, "to=@s-peer kind=ask dl=100 --wait-ack late?", None)
            })
        };
        loop {
            if cmd_deliver(&store, &format!("{sid} landed=4 off=45")) == "OK\n" {
                break;
            }
            aterm_uds::deadline::poll_yield();
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert_eq!(
            cmd_deliver(
                &store,
                &format!("{sid} off=52 from=n-lab kind=expired re=45 dl=100 text=expired")
            ),
            "OK 3\n"
        );
        assert_eq!(
            poster.join().expect("the poster finished"),
            "ERR expired id=4 off=45\n"
        );
        let poster = {
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                TEST_EXPIRY_GRACE_MS.with(|grace| grace.set(Some(WAIT_MAX_MS)));
                cmd_post(
                    &ctx,
                    "to=@s-peer kind=ask dl=100 --wait-ack=200 exact",
                    None,
                )
            })
        };
        loop {
            if cmd_deliver(&store, &format!("{sid} landed=5 off=46")) == "OK\n" {
                break;
            }
            aterm_uds::deadline::poll_yield();
        }
        let started = std::time::Instant::now();
        assert_eq!(
            poster.join().expect("the poster finished"),
            "ERR timeout id=5 off=46\n"
        );
        assert!(
            started.elapsed() < aterm_spec::wait::MUST_HAPPEN,
            "an explicit bound gets no grace: {:?}",
            started.elapsed()
        );
    });
}

#[test]
fn a_decided_ask_owes_a_receipt_on_the_outbox_until_the_bridge_retires_it() {
    with_link(|| {
        let store = new_store();
        let (sid, ctx) = registered(&store);
        deliver(&store, &sid, 10, "h-andrew", "task", "one");
        deliver(&store, &sid, 11, "s-peer@n-lab", "ask", "two");
        deliver(&store, &sid, 12, "s-peer@n-lab", "note", "three");
        let receipts = |store: &Store| -> Vec<String> {
            let reply = cmd_outbox(store, "");
            let (_, mut rest) = reply.split_once('\n').expect("a header");
            let mut out = Vec::new();
            while let Some((line, tail)) = rest.split_once('\n') {
                rest = tail;
                if line.starts_with("receipt ") {
                    out.push(line.to_string());
                } else if let Some((_, n)) = line.rsplit_once(" len=") {
                    let n: usize = n.parse().expect("a post's len=");
                    rest = &rest[n..];
                }
            }
            out
        };
        assert!(receipts(&store).is_empty(), "nothing decided, nothing owed");
        assert_eq!(cmd_inbox_seen(&ctx, "3"), "OK seen=3\n");
        assert_eq!(cmd_inbox_seen(&ctx, "3 handled"), "OK seen=3\n");
        assert!(
            receipts(&store).is_empty(),
            "a bare seen and a note owe nothing"
        );
        assert_eq!(cmd_inbox_seen(&ctx, "2 deferred"), "OK seen=3\n");
        assert_eq!(cmd_inbox_seen(&ctx, "2 deferred"), "OK seen=3\n");
        assert_eq!(cmd_inbox_seen(&ctx, "1 handled"), "OK seen=3\n");
        assert_eq!(cmd_inbox_seen(&ctx, "2 handled"), "OK seen=3\n");
        let owed = receipts(&store);
        assert_eq!(
            owed,
            [
                format!(
                    "receipt sid={sid} rid=1 off=11 verdict=deferred kind=ask from=s-peer@n-lab"
                ),
                format!("receipt sid={sid} rid=2 off=10 verdict=handled kind=task from=h-andrew"),
                format!(
                    "receipt sid={sid} rid=3 off=11 verdict=handled kind=ask from=s-peer@n-lab"
                ),
            ],
            "one per decision, in order; the repeated word owes nothing"
        );
        assert_eq!(
            receipts(&store),
            owed,
            "a PEEK: the next drain lists them again"
        );
        for bad in [
            "receipt=x off=5",
            "receipt=1",
            "receipt=1 off=x",
            "receipt=1 off=5 more=1",
        ] {
            assert_eq!(
                cmd_deliver(&store, &format!("{sid} {bad}")),
                DELIVER_USAGE,
                "{bad}"
            );
        }
        assert_eq!(
            cmd_deliver(&store, &format!("{sid} receipt=1 off=90")),
            "OK\n"
        );
        assert_eq!(
            cmd_deliver(&store, &format!("{sid} receipt=1 off=90")),
            "OK\n",
            "a retried retirement is OK, so the bridge's pin is never stranded"
        );
        assert_eq!(
            cmd_deliver(&store, &format!("{sid} receipt=3 off=-")),
            "OK\n"
        );
        assert_eq!(
            receipts(&store),
            [format!(
                "receipt sid={sid} rid=2 off=10 verdict=handled kind=task from=h-andrew"
            )]
        );
        assert_eq!(cmd_post(&ctx, "to=@s-peer kind=note after", None), "OK 1\n");
        let reply = cmd_outbox(&store, "");
        let post_at = reply.find("post sid=").expect("the post");
        let receipt_at = reply.find("receipt sid=").expect("the receipt");
        assert!(post_at < receipt_at, "{reply}");
        for n in 0..u64::try_from(RECEIPTS_OWED_MAX).expect("fits") + 5 {
            let word = if n % 2 == 0 { "refused" } else { "handled" };
            assert_eq!(cmd_inbox_seen(&ctx, &format!("2 {word}")), "OK seen=3\n");
        }
        let owed = receipts(&store);
        assert_eq!(owed.len(), RECEIPTS_OWED_MAX);
        assert!(
            !owed.iter().any(|l| l.contains(" rid=2 ")),
            "the oldest owed receipt is the one dropped"
        );
    });
}

#[test]
fn inbox_get_at_an_offset_answers_the_ring_or_the_bus_and_the_header_names_the_oldest() {
    with_link(|| {
        let store = new_store();
        let (sid, ctx) = registered(&store);
        attach_up(&store, next_bridge_generation());
        let await_fetch = |off: u64| {
            let deadline = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
            loop {
                let peek = cmd_outbox(&store, "");
                if peek.contains(&format!("fetch sid={sid} off={off}\n")) {
                    return;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "no `fetch off={off}` on the outbox peek within a minute; drain:\n{peek}"
                );
                aterm_uds::deadline::poll_yield();
            }
        };
        let total = RING_CAP as u64 + 8;
        for i in 0..total {
            let off = 1000 + i;
            assert_eq!(
                deliver(
                    &store,
                    &sid,
                    off,
                    "s-peer@n-lab",
                    "note",
                    &format!("body-{off}")
                ),
                format!("OK {}\n", i + 1)
            );
            if i % 60 == 59 {
                cmd_inbox(&ctx, "--meta");
            }
        }
        let newest = 1000 + total - 1;
        let header = cmd_inbox(&ctx, "0 --peek --meta");
        assert!(
            header.contains(&format!(
                " bus_head={newest} oldest_on_bus=@1000 dropped=8 "
            )),
            "the evicted rows' oldest offset is still the oldest on the bus: {header}"
        );
        assert_eq!(
            cmd_inbox_get(&ctx, &format!("@{newest}")),
            format!("OK 9 off={newest} from=s-peer@n-lab kind=note trust=agent\nbody-{newest}")
        );
        let reader = {
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                fetch_waits_a_hang();
                cmd_inbox_get(&ctx, "@1000")
            })
        };
        await_fetch(1000);
        assert_eq!(
            cmd_deliver(
                &store,
                &format!(
                    "{sid} fetched=1000 from=s-peer@n-lab kind=note trust=agent \
                     text=body-1000"
                )
            ),
            "OK\n"
        );
        assert_eq!(
            reader.join().expect("the reader finished"),
            "OK 9 off=1000 from=s-peer@n-lab kind=note trust=agent\nbody-1000"
        );
        assert_eq!(
            cmd_inbox_get(&ctx, "@1000"),
            "OK 9 off=1000 from=s-peer@n-lab kind=note trust=agent\nbody-1000"
        );
        assert!(!cmd_outbox(&store, "").contains("fetch "));
        assert_eq!(cmd_inbox_get(&ctx, "1"), "ERR no such message\n");
        let reader = {
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                fetch_waits_a_hang();
                cmd_inbox_get(&ctx, "@7")
            })
        };
        await_fetch(7);
        assert_eq!(
            cmd_deliver(&store, &format!("{sid} fetched=7 err=no-record")),
            "OK\n"
        );
        assert_eq!(
            reader.join().expect("the reader finished"),
            "ERR no such record off=7\n"
        );
        let reader = {
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                fetch_waits_a_hang();
                cmd_inbox_get(&ctx, "@8")
            })
        };
        await_fetch(8);
        assert_eq!(
            cmd_deliver(&store, &format!("{sid} fetched=8 err=retained floor=40")),
            "OK\n"
        );
        assert_eq!(
            reader.join().expect("the reader finished"),
            "ERR retained-from=40 off=8\n"
        );
        assert_eq!(
            cmd_deliver(&store, &format!("{sid} fetched=8 err=retained floor=x")),
            DELIVER_USAGE
        );
        assert_eq!(cmd_inbox_get(&ctx, "@x"), INBOX_USAGE);

        let reader = {
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                fetch_waits_a_hang();
                cmd_inbox_get(&ctx, "@1002")
            })
        };
        await_fetch(1002);
        let line = |tail: &str| {
            cmd_deliver(
                &store,
                &format!("{sid} fetched=1002 from=s-peer@n-lab kind=note trust=agent {tail}"),
            )
        };
        assert_eq!(line("more=1 text=abc"), "OK\n");
        assert!(
            cmd_outbox(&store, "").contains(&format!("fetch sid={sid} off=1002\n")),
            "a partial answer is still waited on, so still listed"
        );
        assert_eq!(line("at=3 more=1 text=def"), "OK\n");
        assert_eq!(line("at=6 text=gh%20i"), "OK\n");
        assert_eq!(
            reader.join().expect("the reader finished"),
            "OK 10 off=1002 from=s-peer@n-lab kind=note trust=agent\nabcdefgh i"
        );
        let reader = {
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                fetch_waits_a_hang();
                cmd_inbox_get(&ctx, "@1003")
            })
        };
        await_fetch(1003);
        let line = |tail: &str| {
            cmd_deliver(
                &store,
                &format!("{sid} fetched=1003 from=s-peer@n-lab kind=note trust=agent {tail}"),
            )
        };
        assert_eq!(line("more=1 text=abc"), "OK\n");
        assert_eq!(line("at=2 text=xyz"), "ERR fetch out of order\n");
        assert_eq!(
            reader.join().expect("the reader finished"),
            "ERR fetch failed reason=malformed off=1003\n",
            "a refused answer ends the read now, not at its 10 s timeout"
        );
        let reader = {
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                fetch_waits_a_hang();
                cmd_inbox_get(&ctx, "@1004")
            })
        };
        await_fetch(1004);
        let big = BODY_MAX + 44_000;
        assert_eq!(
            cmd_deliver(
                &store,
                &format!(
                    "{sid} fetched=1004 from=s-peer@n-lab kind=note trust=agent len={big} \
                     text=head"
                )
            ),
            "OK\n"
        );
        assert_eq!(
            reader.join().expect("the reader finished"),
            format!(
                "OK 4 off=1004 from=s-peer@n-lab kind=note trust=agent truncated=1 \
                 len={big}\nhead"
            )
        );
        cmd_inbox(&ctx, "--meta");
        assert!(
            cmd_deliver(
                &store,
                &format!("{sid} off=5000 from=s-peer@n-lab kind=note trust=agent len=12 text=cut-")
            )
            .starts_with("OK ")
        );
        let reader = {
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                fetch_waits_a_hang();
                cmd_inbox_get(&ctx, "@5000")
            })
        };
        await_fetch(5000);
        assert_eq!(
            cmd_deliver(
                &store,
                &format!(
                    "{sid} fetched=5000 from=s-peer@n-lab kind=note trust=agent \
                     text=cut-no-more%21"
                )
            ),
            "OK\n"
        );
        assert_eq!(
            reader.join().expect("the reader finished"),
            "OK 12 off=5000 from=s-peer@n-lab kind=note trust=agent\ncut-no-more!"
        );
        assert!(
            cmd_deliver(
                &store,
                &format!(
                    "{sid} off=5001 from=s-peer@n-lab kind=note trust=agent len=40 text=prefix"
                )
            )
            .starts_with("OK ")
        );
        let reader = {
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                fetch_waits_a_hang();
                cmd_inbox_get(&ctx, "@5001")
            })
        };
        await_fetch(5001);
        assert_eq!(
            cmd_deliver(&store, &format!("{sid} fetched=5001 err=unreadable")),
            "OK\n"
        );
        assert_eq!(
            reader.join().expect("the reader finished"),
            "OK 6 off=5001 from=s-peer@n-lab kind=note trust=agent truncated=1 len=40\nprefix"
        );
    });
}

#[test]
fn after_a_log_switch_the_refill_floor_is_the_new_logs() {
    with_link(|| {
        let store = new_store();
        let (sid, ctx) = registered(&store);
        let generation = next_bridge_generation();
        attach_up(&store, generation);
        let tell = |ident: String| cmd_link(&store, &format!("log {ident}"), Some(generation));
        let last_seen = || {
            ctx.timeline
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .since(None)
                .rfind(|e| e.kind == "inbox-seen")
                .map(|e| e.fact.clone())
                .unwrap_or_default()
        };
        assert_eq!(tell(format!("{}.0", "aa".repeat(16))), "OK\n");
        assert_eq!(deliver(&store, &sid, 7, "h-a", "note", "a0"), "OK 1\n");
        assert_eq!(deliver(&store, &sid, 900, "h-a", "note", "a1"), "OK 2\n");
        assert_eq!(cmd_inbox_seen(&ctx, "1"), "OK seen=1\n");
        assert_eq!(last_seen(), "1 off=7 floor=900 seen=1");
        assert_eq!(tell(format!("{}.0", "bb".repeat(16))), "OK forgot=1\n");
        assert_eq!(deliver(&store, &sid, 950, "h-a", "note", "b0"), "OK 3\n");
        assert_eq!(cmd_inbox_seen(&ctx, "1"), "OK seen=1\n");
        assert_eq!(
            last_seen(),
            "1 off=7 floor=950 seen=1",
            "A's unhandled row is no floor on B"
        );
        assert_eq!(cmd_inbox_seen(&ctx, "3"), "OK seen=3\n");
        assert_eq!(last_seen(), "3 off=950 floor=951 seen=3");
    });
}

#[test]
fn a_log_switch_forgets_every_offset_the_endpoint_holds() {
    with_link(|| {
        let store = new_store();
        let (sid, ctx) = registered(&store);
        let generation = next_bridge_generation();
        attach_up(&store, generation);
        let a = format!("{}.0", "aa".repeat(16));
        let b = format!("{}.3", "bb".repeat(16));
        let tell = |ident: &str| cmd_link(&store, &format!("log {ident}"), Some(generation));
        assert_eq!(tell(&a), "OK\n", "the first log forgets nothing");
        assert_eq!(
            deliver(&store, &sid, 7, "s-peer@n-lab", "note", "on-a"),
            "OK 1\n"
        );
        assert_eq!(
            deliver(&store, &sid, 7, "s-peer@n-lab", "note", "on-a"),
            "OK 1\n",
            "on one log an offset is one record"
        );
        assert_eq!(tell(&a), "OK\n", "the same log again forgets nothing");
        let parked = {
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                fetch_waits_a_hang();
                cmd_inbox_get(&ctx, "@1000")
            })
        };
        let deadline = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
        while !cmd_outbox(&store, "").contains(&format!("fetch sid={sid} off=1000\n")) {
            assert!(
                std::time::Instant::now() < deadline,
                "the read never parked"
            );
            aterm_uds::deadline::poll_yield();
        }

        assert_eq!(tell(&b), "OK forgot=1\n");
        assert_eq!(
            parked.join().expect("the reader finished"),
            "ERR fetch failed reason=log-replaced off=1000\n"
        );
        assert_eq!(
            deliver(&store, &sid, 7, "s-peer@n-lab", "note", "on-b"),
            "OK 2\n",
            "B's record at 7 is not A's"
        );
        assert_eq!(
            cmd_inbox_get(&ctx, "@7"),
            "OK 4 off=7 from=s-peer@n-lab kind=note trust=agent\non-b"
        );
        assert!(
            cmd_inbox(&ctx, "0 --peek --meta").contains(" bus_head=7 oldest_on_bus=@7 "),
            "the span is B's"
        );

        assert_eq!(cmd_link(&store, "log nope", Some(generation)), LINK_USAGE);
        assert_eq!(
            cmd_link(&store, &format!("log {a} extra"), Some(generation)),
            LINK_USAGE
        );
        let stale = next_bridge_generation();
        assert_eq!(
            cmd_link(&store, &format!("log {a}"), Some(stale)),
            "OK stale=1\n"
        );
        assert_eq!(
            deliver(&store, &sid, 7, "s-peer@n-lab", "note", "on-b"),
            "OK 2\n",
            "a stale lane forgot nothing"
        );
    });
}

#[test]
fn log_switch_retires_old_receipt_debt_without_erasing_the_mail() {
    with_link(|| {
        let store = new_store();
        let (sid, ctx) = registered(&store);
        let generation = next_bridge_generation();
        attach_up(&store, generation);
        let tell = |hex: &str| {
            cmd_link(
                &store,
                &format!("log {}.0", hex.repeat(32)),
                Some(generation),
            )
        };
        assert_eq!(tell("a"), "OK\n");
        assert_eq!(
            deliver(&store, &sid, 7, "s-peer@n-lab", "ask", "old"),
            "OK 1\n"
        );
        assert_eq!(cmd_inbox_seen(&ctx, "1 handled"), "OK seen=1\n");
        assert!(cmd_outbox(&store, "").contains("receipt sid="));
        assert_eq!(tell("b"), "OK forgot=1\n");
        assert!(
            !cmd_outbox(&store, "").contains("receipt sid="),
            "A's receipt cannot acknowledge an unrelated record at that offset on B"
        );
        assert_eq!(
            cmd_inbox_get(&ctx, "1"),
            "OK 3\nold",
            "historical mail remains readable"
        );
        assert_eq!(
            ctx.fabric.lock().rows[0].decision.as_deref(),
            Some("handled")
        );
        assert_eq!(
            deliver(&store, &sid, 7, "s-peer@n-lab", "ask", "new"),
            "OK 2\n"
        );
        assert_eq!(cmd_inbox_seen(&ctx, "2 handled"), "OK seen=2\n");
        assert!(
            cmd_outbox(&store, "")
                .contains(&format!("receipt sid={sid} rid=2 off=7 verdict=handled")),
            "B's receipt remains owed"
        );
    });
}

#[test]
fn log_switch_a_later_historical_verdict_is_local() {
    with_link(|| {
        let store = new_store();
        let (sid, ctx) = registered(&store);
        let generation = next_bridge_generation();
        attach_up(&store, generation);
        let tell = |hex: &str| {
            cmd_link(
                &store,
                &format!("log {}.0", hex.repeat(32)),
                Some(generation),
            )
        };
        assert_eq!(tell("a"), "OK\n");
        assert_eq!(
            deliver(&store, &sid, 7, "s-peer@n-lab", "task", "old"),
            "OK 1\n"
        );
        assert_eq!(tell("b"), "OK forgot=1\n");
        assert_eq!(cmd_inbox_seen(&ctx, "1 handled"), "OK seen=1\n");
        assert!(
            !cmd_outbox(&store, "").contains("receipt sid="),
            "deciding historical mail must not publish a receipt on the new log"
        );
        assert_eq!(
            ctx.fabric.lock().rows[0].decision.as_deref(),
            Some("handled")
        );
        assert_eq!(
            deliver(&store, &sid, 8, "s-peer@n-lab", "task", "new"),
            "OK 2\n"
        );
        assert_eq!(cmd_inbox_seen(&ctx, "2 handled"), "OK seen=2\n");
        assert!(cmd_outbox(&store, "").contains("off=8 verdict=handled"));
    });
}

#[test]
fn log_switch_receipt_waits_do_not_match_another_logs_offset() {
    with_link(|| {
        let store = new_store();
        let (sid, ctx) = registered(&store);
        let generation = next_bridge_generation();
        attach_up(&store, generation);
        let tell = |hex: &str| {
            cmd_link(
                &store,
                &format!("log {}.0", hex.repeat(32)),
                Some(generation),
            )
        };
        let ack = || {
            cmd_deliver(
                &store,
                &format!(
                    "{sid} off=20 from=s-peer@n-lab kind=ack trust=agent re=7 verdict=handled text=receipt"
                ),
            )
        };
        assert_eq!(tell("a"), "OK\n");
        assert_eq!(cmd_post(&ctx, "to=@s-peer kind=note old", None), "OK 1\n");
        assert_eq!(cmd_outbox_sent(&store, &format!("{sid} 1 off=7")), "OK\n");
        assert_eq!(ack(), "OK 1\n");
        assert_eq!(
            wait_receipt(&ctx, 1, 7, "", 0),
            "OK 1 off=7 ack=handled msg=1\n",
            "same-log receipt is valid"
        );
        assert_eq!(tell("b"), "OK forgot=1\n");
        assert_eq!(cmd_post(&ctx, "to=@s-peer kind=note new", None), "OK 2\n");
        assert_eq!(cmd_outbox_sent(&store, &format!("{sid} 2 off=7")), "OK\n");
        assert_eq!(
            wait_receipt(&ctx, 2, 7, "", 0),
            "ERR timeout id=2 off=7\n",
            "A's receipt cannot satisfy B's post"
        );
        assert_eq!(
            cmd_await_inbox(&ctx, &["re=7"], 0),
            "OK timeout\n",
            "offset lookup is scoped to the current log too"
        );
        assert_eq!(ack(), "OK 2\n");
        assert_eq!(
            wait_receipt(&ctx, 2, 7, "", 0),
            "OK 2 off=7 ack=handled msg=2\n"
        );
        assert_eq!(
            wait_receipt(&ctx, 1, 7, "", 0),
            "ERR log-replaced id=1 off=7\n",
            "B's receipt cannot satisfy A's post"
        );
    });
}

#[test]
fn log_switch_keeps_a_queued_posts_new_log_thread() {
    with_link(|| {
        let store = new_store();
        let (sid, ctx) = registered(&store);
        let generation = next_bridge_generation();
        attach_up(&store, generation);
        let tell = |hex: &str| {
            cmd_link(
                &store,
                &format!("log {}.0", hex.repeat(32)),
                Some(generation),
            )
        };
        assert_eq!(tell("a"), "OK\n");
        assert_eq!(
            cmd_post(&ctx, "to=@s-peer kind=note queued", None),
            "OK 1\n"
        );
        assert_eq!(tell("b"), "OK forgot=1\n");
        assert!(
            cmd_outbox(&store, "").contains("post sid="),
            "an unlanded new message remains queued"
        );
        assert_eq!(cmd_outbox_sent(&store, &format!("{sid} 1 off=7")), "OK\n");
        assert_eq!(
            cmd_deliver(
                &store,
                &format!("{sid} off=8 from=s-peer@n-lab kind=answer trust=agent re=7 text=reply")
            ),
            "OK 1\n"
        );
        assert_eq!(
            ctx.fabric.lock().rows.back().unwrap().re_id,
            Some(1),
            "the post landed on B even though it was queued while A was connected"
        );
    });
}

#[test]
fn log_switch_a_queued_reply_cannot_reuse_a_historical_reference() {
    with_link(|| {
        let store = new_store();
        let (sid, ctx) = registered(&store);
        let generation = next_bridge_generation();
        attach_up(&store, generation);
        let tell = |hex: &str| {
            cmd_link(
                &store,
                &format!("log {}.0", hex.repeat(32)),
                Some(generation),
            )
        };
        assert_eq!(tell("a"), "OK\n");
        assert_eq!(
            deliver(&store, &sid, 7, "s-peer@n-lab", "ask", "old"),
            "OK 1\n"
        );
        assert_eq!(
            cmd_post(&ctx, "to=@s-peer kind=answer re=7 queued-reply", None),
            "OK 1\n"
        );
        assert_eq!(tell("b"), "OK forgot=1\n");
        assert_eq!(
            wait_landing(&ctx, 1, std::time::Instant::now()).err(),
            Some("ERR log-replaced id=1\n".to_string()),
            "the queued reply is reported dead, not published as a reply to B's unrelated offset"
        );
        assert!(!cmd_outbox(&store, "").contains("post sid="));
        assert_eq!(cmd_inbox_get(&ctx, "1"), "OK 3\nold");
    });
}

#[cfg(unix)]
#[test]
fn log_switch_waits_for_the_commit_receipt_fence_and_retries() {
    with_link(|| {
        let store = new_store();
        let (sid, ctx) = registered(&store);
        let generation = next_bridge_generation();
        attach_up(&store, generation);
        let tell = |hex: &str| {
            cmd_link(
                &store,
                &format!("log {}.0", hex.repeat(32)),
                Some(generation),
            )
        };
        assert_eq!(tell("a"), "OK\n");
        assert_eq!(
            deliver(&store, &sid, 7, "s-peer@n-lab", "ask", "old"),
            "OK 1\n"
        );
        assert_eq!(cmd_inbox_seen(&ctx, "1 handled"), "OK seen=1\n");
        let before = ctx.fabric.carry_serial();
        ctx.fabric.fence_hold(true);
        assert_eq!(
            tell("b"),
            HOLD_BUSY,
            "a committing successor still owns the carried receipt"
        );
        assert_eq!(
            ctx.fabric.carry_serial(),
            before,
            "no carried state changed under the fence"
        );
        assert!(cmd_outbox(&store, "").contains("receipt sid="));
        ctx.fabric.lift_hold_fence();
        assert_eq!(
            tell("b"),
            "OK forgot=1\n",
            "the same identity retries the unapplied switch"
        );
        assert!(!cmd_outbox(&store, "").contains("receipt sid="));
        assert_eq!(tell("b"), "OK\n", "a completed switch is idempotent");
    });
}

#[test]
fn log_switch_a_successors_first_attach_checks_the_carried_log() {
    let carry = with_link(|| {
        let store = new_store();
        let (sid, ctx) = registered(&store);
        let generation = next_bridge_generation();
        attach_up(&store, generation);
        assert_eq!(
            cmd_link(
                &store,
                &format!("log {}.0", "a".repeat(32)),
                Some(generation)
            ),
            "OK\n"
        );
        assert_eq!(
            deliver(&store, &sid, 7, "s-peer@n-lab", "ask", "old"),
            "OK 1\n"
        );
        assert_eq!(cmd_inbox_seen(&ctx, "1 handled"), "OK seen=1\n");
        assert_eq!(
            cmd_post(&ctx, "to=@s-peer kind=answer re=7 reply", None),
            "OK 1\n"
        );
        assert_eq!(
            cmd_post(&ctx, "to=@s-peer kind=note independent", None),
            "OK 2\n"
        );
        let mut budget = CARRY_MAIL_BYTES;
        ctx.fabric.handoff_carry(&mut budget)
    });
    let wire = aterm_toml::to_string(&carry).expect("outgoing carry");
    let legacy = wire
        .lines()
        .filter(|l| !l.starts_with("bus_log ="))
        .collect::<Vec<_>>()
        .join("\n");
    let invalid = format!("bus_log = \"not-an-identity\"\n{legacy}");
    for (name, source, hex, switched) in [
        ("same log", wire.as_str(), "a", false),
        ("different log", wire.as_str(), "b", true),
        ("legacy unknown log", legacy.as_str(), "a", true),
        ("invalid carried log", invalid.as_str(), "a", true),
    ] {
        let carried: FabricCarry = aterm_toml::from_str(source).expect("incoming carry");
        with_link(|| {
            let store = new_store();
            let (_sid, ctx) = registered(&store);
            ctx.fabric.seed_carry(&carried, &ctx.timeline);
            let generation = next_bridge_generation();
            attach_up(&store, generation);
            assert_eq!(
                cmd_link(
                    &store,
                    &format!("log {}.0", hex.repeat(32)),
                    Some(generation)
                ),
                if switched { "OK forgot=1\n" } else { "OK\n" },
                "{name}"
            );
            let outbox = cmd_outbox(&store, "");
            assert_eq!(
                outbox.contains("receipt sid="),
                !switched,
                "{name}: {outbox}"
            );
            assert_eq!(outbox.contains("id=1 "), !switched, "{name}: {outbox}");
            assert!(
                outbox.contains("id=2 ") && outbox.contains("independent"),
                "unreferenced carried mail stays queued ({name}): {outbox}"
            );
        });
    }
}

#[test]
fn log_switch_a_late_fresh_session_keeps_same_log_receipts_and_dedup() {
    with_link(|| {
        let store = new_store();
        registered(&store);
        let generation = next_bridge_generation();
        attach_up(&store, generation);
        let tell = || {
            cmd_link(
                &store,
                &format!("log {}.0", "a".repeat(32)),
                Some(generation),
            )
        };
        assert_eq!(tell(), "OK\n");
        let late = test_handle(2);
        let sid = late.sid.as_str().to_string();
        let ctx = late.ctx.clone();
        register_on_log(&ctx, || {
            store
                .write()
                .unwrap_or_else(|p| p.into_inner())
                .register(late);
        });
        assert_eq!(
            deliver(&store, &sid, 7, "s-peer@n-lab", "ask", "live"),
            "OK 1\n"
        );
        assert_eq!(cmd_inbox_seen(&ctx, "1 handled"), "OK seen=1\n");
        assert_eq!(
            cmd_post(&ctx, "to=@s-peer kind=answer re=7 reply", None),
            "OK 1\n"
        );
        assert_eq!(tell(), "OK\n", "reattaching to A keeps A's live references");
        let outbox = cmd_outbox(&store, "");
        assert!(outbox.contains("off=7 verdict=handled"), "{outbox}");
        assert!(
            outbox.contains("re=7") && outbox.contains("reply"),
            "{outbox}"
        );
        assert_eq!(
            deliver(&store, &sid, 7, "s-peer@n-lab", "ask", "live"),
            "OK 1\n"
        );
        assert_eq!(
            ctx.fabric.lock().rows.len(),
            1,
            "same-log replay is deduplicated"
        );
        assert_eq!(
            cmd_inbox_get(&ctx, "@7"),
            "OK 4 off=7 from=s-peer@n-lab kind=ask trust=agent\nlive"
        );
    });
}

#[test]
fn log_switch_a_late_adoption_checks_carry_before_its_first_outbox() {
    let carry = with_link(|| {
        let store = new_store();
        let (sid, ctx) = registered(&store);
        let generation = next_bridge_generation();
        attach_up(&store, generation);
        assert_eq!(
            cmd_link(
                &store,
                &format!("log {}.0", "a".repeat(32)),
                Some(generation)
            ),
            "OK\n"
        );
        assert_eq!(
            deliver(&store, &sid, 7, "s-peer@n-lab", "ask", "old"),
            "OK 1\n"
        );
        assert_eq!(cmd_inbox_seen(&ctx, "1 handled"), "OK seen=1\n");
        assert_eq!(
            cmd_post(&ctx, "to=@s-peer kind=answer re=7 reply", None),
            "OK 1\n"
        );
        assert_eq!(
            cmd_post(&ctx, "to=@s-peer kind=note independent", None),
            "OK 2\n"
        );
        ctx.fabric.handoff_carry(&mut CARRY_MAIL_BYTES.clone())
    });
    let wire = aterm_toml::to_string(&carry).expect("outgoing carry");
    let legacy = wire
        .lines()
        .filter(|line| !line.starts_with("bus_log ="))
        .collect::<Vec<_>>()
        .join("\n");
    for (name, source, target, same) in [
        ("same known log", wire.as_str(), "a", true),
        ("replacement log", wire.as_str(), "b", false),
        ("unknown legacy log", legacy.as_str(), "b", false),
    ] {
        with_link(|| {
            let store = new_store();
            registered(&store);
            let generation = next_bridge_generation();
            attach_up(&store, generation);
            assert_eq!(
                cmd_link(
                    &store,
                    &format!("log {}.0", target.repeat(32)),
                    Some(generation)
                ),
                "OK\n"
            );
            let late = test_handle(2);
            let ctx = late.ctx.clone();
            let incoming: FabricCarry = aterm_toml::from_str(source).expect("incoming carry");
            ctx.fabric.seed_carry(&incoming, &ctx.timeline);
            register_on_log(&ctx, || {
                store
                    .write()
                    .unwrap_or_else(|p| p.into_inner())
                    .register(late);
            });
            let outbox = cmd_outbox(&store, "");
            assert_eq!(outbox.contains("receipt sid="), same, "{name}: {outbox}");
            assert_eq!(outbox.contains("re=7"), same, "{name}: {outbox}");
            assert!(outbox.contains("independent"), "{name}: {outbox}");
            assert_eq!(
                wait_landing(&ctx, 1, std::time::Instant::now()).err(),
                if same {
                    Some("ERR timeout id=1\n".to_string())
                } else {
                    Some("ERR log-replaced id=1\n".to_string())
                },
                "{name}"
            );
        });
    }
}

#[test]
fn a_post_queued_across_a_log_switch_threads_its_reply_on_the_new_log() {
    with_link(|| {
        let store = new_store();
        let (sid, ctx) = registered(&store);
        let generation = next_bridge_generation();
        attach_up(&store, generation);
        let tell = |ident: String| cmd_link(&store, &format!("log {ident}"), Some(generation));
        assert_eq!(tell(format!("{}.0", "aa".repeat(16))), "OK\n");
        assert_eq!(cmd_post(&ctx, "to=@s-peer kind=note on a", None), "OK 1\n");
        assert_eq!(
            cmd_deliver(&store, &format!("{sid} landed=1 off=70")),
            "OK\n"
        );
        assert_eq!(cmd_post(&ctx, "to=@s-peer kind=note on b", None), "OK 2\n");
        assert_eq!(tell(format!("{}.1", "aa".repeat(16))), "OK forgot=1\n");
        assert_eq!(
            cmd_deliver(&store, &format!("{sid} landed=2 off=70")),
            "OK\n"
        );
        assert_eq!(
            cmd_deliver(
                &store,
                &format!("{sid} off=71 from=s-peer@n-lab kind=note re=70 trust=agent text=re")
            ),
            "OK 1\n"
        );
        let listing = cmd_inbox(&ctx, "--peek --meta");
        let reply = listing
            .lines()
            .find(|l| l.contains(" re=70"))
            .unwrap_or_else(|| panic!("no reply row: {listing}"));
        assert!(reply.contains(" re=70 re-id=2 "), "{reply}");
    });
}

fn await_parked(store: &Store, sid: &str, off: u64) {
    let deadline = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    loop {
        let peek = cmd_outbox(store, "");
        if peek.contains(&format!("fetch sid={sid} off={off}\n")) {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "no `fetch off={off}` on the outbox peek within a minute; drain:\n{peek}"
        );
        aterm_uds::deadline::poll_yield();
    }
}

fn last_inbox_seen(ctx: &SessionCtx) -> String {
    ctx.timeline
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .since(None)
        .rfind(|e| e.kind == "inbox-seen")
        .map(|e| e.fact.clone())
        .unwrap_or_default()
}

#[test]
fn a_moved_generation_keeps_what_names_a_record_below_the_fork() {
    let a0 = format!("{}.0", "aa".repeat(16));
    let a1 = format!("{}.1", "aa".repeat(16));
    for lineage in [true, false] {
        with_link(|| {
            stale_after_a_hang();
            let store = new_store();
            let (sid, ctx) = registered(&store);
            let generation = next_bridge_generation();
            attach_up(&store, generation);
            let case = format!("lineage={lineage}");
            let tell = |line: &str| cmd_link(&store, &format!("log {line}"), Some(generation));
            let read = |off: u64| {
                let ctx = ctx.clone();
                std::thread::spawn(move || {
                    fetch_waits_a_hang();
                    cmd_inbox_get(&ctx, &format!("@{off}"))
                })
            };
            let fetched = |off: u64, text: &str| {
                cmd_deliver(
                    &store,
                    &format!(
                        "{sid} fetched={off} from=s-peer@n-lab kind=note trust=agent text={text}"
                    ),
                )
            };
            let ack = |off: u64, re: u64| {
                cmd_deliver(
                    &store,
                    &format!(
                        "{sid} off={off} from=s-peer@n-lab kind=ack trust=agent re={re} \
                         verdict=handled text=ack"
                    ),
                )
            };
            let reply_row = |re: u64| {
                let listing = cmd_inbox(&ctx, "--peek --meta");
                listing
                    .lines()
                    .find(|l| l.contains(&format!(" re={re}")))
                    .unwrap_or_else(|| panic!("{case}: no row re={re}: {listing}"))
                    .to_string()
            };

            assert_eq!(tell(&a0), "OK\n", "{case}");
            assert_eq!(
                deliver(&store, &sid, 7, "s-peer@n-lab", "ask", "below"),
                "OK 1\n"
            );
            assert_eq!(
                deliver(&store, &sid, 40, "s-peer@n-lab", "ask", "above"),
                "OK 2\n"
            );
            assert_eq!(cmd_inbox_seen(&ctx, "1 handled"), "OK seen=1\n");
            assert_eq!(cmd_inbox_seen(&ctx, "2 handled"), "OK seen=2\n");
            assert_eq!(
                cmd_post(&ctx, "to=@s-peer kind=answer re=7 reply-below", None),
                "OK 1\n"
            );
            assert_eq!(
                cmd_post(&ctx, "to=@s-peer kind=answer re=40 reply-above", None),
                "OK 2\n"
            );
            for (id, off) in [(3, 9), (4, 41)] {
                assert_eq!(
                    cmd_post(&ctx, &format!("to=@s-peer kind=note landed-at-{off}"), None),
                    format!("OK {id}\n")
                );
                assert_eq!(
                    cmd_deliver(&store, &format!("{sid} landed={id} off={off}")),
                    "OK\n"
                );
            }
            let hang_ms = u64::try_from(aterm_spec::wait::MUST_HAPPEN.as_millis())
                .expect("a minute in milliseconds");
            let waiter = {
                let ctx = ctx.clone();
                std::thread::spawn(move || wait_receipt(&ctx, 3, 9, "", hang_ms))
            };
            for off in [3, 44] {
                let reader = read(off);
                await_parked(&store, &sid, off);
                assert_eq!(fetched(off, &format!("old-{off}")), "OK\n");
                let answer = reader.join().expect("the reader finished");
                assert!(answer.ends_with(&format!("\nold-{off}")), "{answer}");
            }
            let parked_below = read(5);
            await_parked(&store, &sid, 5);
            let parked_above = read(45);
            await_parked(&store, &sid, 45);

            let moved = if lineage {
                format!("{a1} continues=20 was={a0}")
            } else {
                a1.clone()
            };
            assert_eq!(tell(&moved), "OK forgot=1\n", "{case}");
            assert_eq!(tell(&a1), "OK\n", "{case}: the move is applied once");

            let header = cmd_inbox(&ctx, "0 --peek --meta");
            let span = if lineage {
                " bus_head=7 oldest_on_bus=@3 "
            } else {
                " bus_head=0 oldest_on_bus=- "
            };
            assert!(header.contains(span), "{case}: {header}");
            assert_eq!(cmd_inbox_seen(&ctx, "2"), "OK seen=2\n");
            assert_eq!(
                last_inbox_seen(&ctx),
                if lineage {
                    "2 off=40 floor=20 seen=2"
                } else {
                    "2 off=40 floor=- seen=2"
                },
                "{case}"
            );

            assert_eq!(
                parked_above.join().expect("the reader finished"),
                "ERR fetch failed reason=log-replaced off=45\n",
                "{case}"
            );
            if lineage {
                await_parked(&store, &sid, 5);
                assert_eq!(fetched(5, "kept-5"), "OK\n");
                let answer = parked_below.join().expect("the reader finished");
                assert!(answer.ends_with("\nkept-5"), "{case}: {answer}");
                let cached = cmd_inbox_get(&ctx, "@3");
                assert!(
                    cached.ends_with("\nold-3"),
                    "{case}: answered once, kept: {cached}"
                );
            } else {
                assert_eq!(
                    parked_below.join().expect("the reader finished"),
                    "ERR fetch failed reason=log-replaced off=5\n",
                    "{case}"
                );
            }
            let again = read(44);
            await_parked(&store, &sid, 44);
            assert_eq!(fetched(44, "new-44"), "OK\n");
            let answer = again.join().expect("the reader finished");
            assert!(
                answer.ends_with("\nnew-44"),
                "{case}: read again on the new generation: {answer}"
            );

            let outbox = cmd_outbox(&store, "");
            assert_eq!(outbox.contains("reply-below"), lineage, "{case}: {outbox}");
            assert!(!outbox.contains("reply-above"), "{case}: {outbox}");
            assert_eq!(
                wait_landing(&ctx, 1, std::time::Instant::now()).err(),
                Some(if lineage {
                    "ERR timeout id=1\n".to_string()
                } else {
                    "ERR log-replaced id=1\n".to_string()
                }),
                "{case}"
            );
            assert_eq!(
                wait_landing(&ctx, 2, std::time::Instant::now()).err(),
                Some("ERR log-replaced id=2\n".to_string()),
                "{case}"
            );
            assert_eq!(
                outbox.contains(&format!("receipt sid={sid} rid=1 off=7 verdict=handled")),
                lineage,
                "{case}: {outbox}"
            );
            assert!(!outbox.contains(" rid=2 "), "{case}: {outbox}");
            let earlier = cmd_outbox(&store, "earlier-receipts=1");
            assert!(
                earlier.contains(
                    " rid=2 off=40 verdict=handled kind=ask from=s-peer@n-lab log=earlier\n"
                ),
                "{case}: {earlier}"
            );
            assert_eq!(
                earlier.contains(
                    " rid=1 off=7 verdict=handled kind=ask from=s-peer@n-lab log=earlier\n"
                ),
                !lineage,
                "{case}: {earlier}"
            );

            assert_eq!(ack(21, 9), "OK 3\n", "{case}");
            assert_eq!(ack(22, 41), "OK 4\n", "{case}");
            assert_eq!(
                reply_row(9).contains(" re=9 re-id=3 "),
                lineage,
                "{case}: {}",
                reply_row(9)
            );
            assert!(!reply_row(41).contains(" re-id="), "{}", reply_row(41));
            assert_eq!(
                waiter.join().expect("the waiter finished"),
                if lineage {
                    "OK 3 off=9 ack=handled msg=3\n"
                } else {
                    "ERR log-replaced id=3 off=9\n"
                },
                "{case}"
            );
            assert_eq!(
                wait_receipt(&ctx, 4, 41, "", 0),
                "ERR log-replaced id=4 off=41\n",
                "{case}"
            );

            assert_eq!(
                deliver(&store, &sid, 7, "s-peer@n-lab", "ask", "below"),
                if lineage { "OK 1\n" } else { "OK 5\n" },
                "{case}"
            );
            assert_eq!(
                deliver(&store, &sid, 40, "s-peer@n-lab", "ask", "new-40"),
                if lineage { "OK 5\n" } else { "OK 6\n" },
                "{case}"
            );
        });
    }
}

#[test]
fn across_two_moves_an_offset_is_kept_only_below_both_forks() {
    with_link(|| {
        let store = new_store();
        let (sid, ctx) = registered(&store);
        let generation = next_bridge_generation();
        attach_up(&store, generation);
        let a = |g: u64| format!("{}.{g}", "aa".repeat(16));
        let tell = |line: String| cmd_link(&store, &format!("log {line}"), Some(generation));
        let land = |id: u64, off: u64| {
            assert_eq!(
                cmd_post(&ctx, &format!("to=@s-peer kind=note on-{id}"), None),
                format!("OK {id}\n")
            );
            assert_eq!(
                cmd_deliver(&store, &format!("{sid} landed={id} off={off}")),
                "OK\n"
            );
        };
        let receipt_at_35 = || {
            (
                cmd_outbox(&store, "").contains(" rid=1 off=35 "),
                cmd_outbox(&store, "earlier-receipts=1").contains(
                    " rid=1 off=35 verdict=handled kind=ask from=s-peer@n-lab log=earlier\n",
                ),
            )
        };

        assert_eq!(tell(a(0)), "OK\n");
        land(1, 40);
        assert_eq!(
            deliver(&store, &sid, 40, "s-peer@n-lab", "note", "a0-at-40"),
            "OK 1\n"
        );
        assert_eq!(
            deliver(&store, &sid, 35, "s-peer@n-lab", "ask", "a0-at-35"),
            "OK 2\n"
        );
        assert_eq!(cmd_inbox_seen(&ctx, "2 handled"), "OK seen=2\n");
        assert_eq!(receipt_at_35(), (true, false), "owed on A.0");
        let (watch_35, watch_25) = {
            let mut inbox = ctx.fabric.lock();
            (inbox.watch(35, true), inbox.watch(25, true))
        };
        assert_eq!(
            tell(format!("{} continues=30 was={}", a(1), a(0))),
            "OK forgot=1\n"
        );
        assert_eq!(receipt_at_35(), (false, true), "35 is at or above 30");
        land(2, 40);
        assert_eq!(
            deliver(&store, &sid, 40, "s-peer@n-lab", "note", "a1-at-40"),
            "OK 3\n",
            "A.1's record at 40 is not A.0's"
        );
        assert_eq!(
            tell(format!("{} continues=50 was={}", a(2), a(1))),
            "OK forgot=1\n"
        );

        assert_eq!(
            cmd_deliver(
                &store,
                &format!("{sid} off=55 from=s-peer@n-lab kind=answer trust=agent re=40 text=re")
            ),
            "OK 4\n"
        );
        let listing = cmd_inbox(&ctx, "--peek --meta");
        let reply = listing
            .lines()
            .find(|l| l.contains(" re=40"))
            .unwrap_or_else(|| panic!("no reply row: {listing}"));
        assert!(
            reply.contains(" re=40 re-id=2 "),
            "the reply answers A.1's post, which A.2 still holds: {reply}"
        );
        assert_eq!(
            cmd_inbox_get(&ctx, "@40"),
            "OK 8 off=40 from=s-peer@n-lab kind=note trust=agent\na1-at-40",
            "A.1's row, not A.0's"
        );
        assert_eq!(
            deliver(&store, &sid, 40, "s-peer@n-lab", "note", "a1-at-40"),
            "OK 3\n",
            "A.1's record handed again on A.2 is the row it was"
        );
        assert_eq!(
            receipt_at_35(),
            (false, true),
            "A.2 holds 35 below its own fork, but not A.0's record there"
        );
        assert!(!watch_35.on_log(), "a wait on A.0's 35 ends: taken at 30");
        assert!(watch_25.on_log(), "a wait on A.0's 25 goes on: below both");
        drop((watch_35, watch_25));
        let _armed = ctx.fabric.lock().watch(1, true);
        assert_eq!(ctx.fabric.lock().watches.len(), 1);
    });
}

#[test]
fn a_session_ahead_of_the_bridges_witness_keeps_below_the_chains_fork() {
    with_link(|| {
        let store = new_store();
        let (sid, ctx) = registered(&store);
        let generation = next_bridge_generation();
        attach_up(&store, generation);
        let a = |g: u64| format!("{}.{g}", "aa".repeat(16));
        let tell = |line: String| cmd_link(&store, &format!("log {line}"), Some(generation));
        assert_eq!(tell(a(0)), "OK\n");
        assert_eq!(
            deliver(&store, &sid, 10, "s-peer@n-lab", "note", "a0-at-10"),
            "OK 1\n"
        );
        assert_eq!(
            deliver(&store, &sid, 35, "s-peer@n-lab", "ask", "a0-at-35"),
            "OK 2\n"
        );
        assert_eq!(
            deliver(&store, &sid, 40, "s-peer@n-lab", "note", "a0-at-40"),
            "OK 3\n"
        );
        assert_eq!(cmd_inbox_seen(&ctx, "2 handled"), "OK seen=2\n");
        assert_eq!(
            tell(format!("{} continues=50 was={}", a(1), a(0))),
            "OK forgot=1\n"
        );
        assert_eq!(
            tell(format!("{} continues=30 was={}", a(2), a(0))),
            "OK forgot=1\n"
        );
        assert_eq!(
            deliver(&store, &sid, 10, "s-peer@n-lab", "note", "a0-at-10"),
            "OK 1\n",
            "below the chain's fork: the row it was"
        );
        assert_eq!(
            deliver(&store, &sid, 40, "s-peer@n-lab", "note", "a2-at-40"),
            "OK 4\n",
            "at or above it: A.2's record at 40 is a new row"
        );
        assert!(
            cmd_outbox(&store, "earlier-receipts=1")
                .contains(" rid=1 off=35 verdict=handled kind=ask from=s-peer@n-lab log=earlier\n"),
            "the receipt at 35 is an earlier log's"
        );

        let other = new_store();
        let (other_sid, _other_ctx) = registered(&other);
        let other_link = |line: String| cmd_link(&other, &format!("log {line}"), Some(generation));
        let b0 = format!("{}.0", "bb".repeat(16));
        assert!(other_link(b0).starts_with("OK"));
        assert_eq!(
            deliver(&other, &other_sid, 10, "s-peer@n-lab", "note", "b0-at-10"),
            "OK 1\n"
        );
        assert_eq!(
            other_link(format!("{} continues=30 was={}", a(3), a(0))),
            "OK forgot=1\n"
        );
        assert_eq!(
            deliver(&other, &other_sid, 10, "s-peer@n-lab", "note", "a3-at-10"),
            "OK 2\n",
            "a session on another log keeps nothing"
        );
    });
}

#[test]
fn link_log_continues_takes_only_an_earlier_generation_of_the_same_log() {
    with_link(|| {
        let store = new_store();
        let (sid, _ctx) = registered(&store);
        let generation = next_bridge_generation();
        attach_up(&store, generation);
        let a = |g: u64| format!("{}.{g}", "aa".repeat(16));
        let b0 = format!("{}.0", "bb".repeat(16));
        let tell = |line: String| cmd_link(&store, &format!("log {line}"), Some(generation));
        let at_seven = || deliver(&store, &sid, 7, "s-peer@n-lab", "note", "seven");
        assert_eq!(tell(a(0)), "OK\n");
        assert_eq!(at_seven(), "OK 1\n");
        for line in [
            format!("{} continues=20", a(1)),
            format!("{} was={}", a(1), a(0)),
            format!("{} continues=x was={}", a(1), a(0)),
            format!("{} continues=-1 was={}", a(1), a(0)),
            format!("{} continues=20 was={b0}", a(1)),
            format!("{} continues=20 was={}", a(1), a(1)),
            format!("{} continues=20 was={}", a(1), a(2)),
            format!("{} continues=20 was=nope", a(1)),
            format!("{} continues=20 continues=21 was={}", a(1), a(0)),
            format!("{} continues=20 was={} was={}", a(1), a(0), a(0)),
            format!("{} continues=20 was={} extra", a(1), a(0)),
        ] {
            assert_eq!(tell(line.clone()), LINK_USAGE, "{line}");
            assert_eq!(at_seven(), "OK 1\n", "nothing forgotten by {line}");
        }
        let stale = next_bridge_generation();
        assert_eq!(
            cmd_link(
                &store,
                &format!("log {} continues=20 was={}", a(1), a(0)),
                Some(stale)
            ),
            "OK stale=1\n"
        );
        assert_eq!(at_seven(), "OK 1\n", "a stale lane forgot nothing");
        assert_eq!(
            tell(format!("{} was={} continues=20", a(1), a(0))),
            "OK forgot=1\n"
        );
        assert_eq!(at_seven(), "OK 1\n", "7 is below the fork");
    });
}

/// A log identity is astream's value, not its text: a bridge that names the clock this
/// endpoint holds with a zero-padded generation (`<id>.01`, or padded past the 20 digits of
/// `u64::MAX`) names the same clock, so nothing is forgotten, and a move from a padded
/// `was=` keeps what is below its fork.
///
/// NEGATIVE CONTROL: each told identity differs from the held one as text, so an endpoint
/// that kept its clock as a string (the copy of the grammar before astream-wire's
/// `LogIdentity`) read every one of them as another log and forgot.
#[test]
fn a_padded_generation_told_by_link_log_is_the_clock_already_held() {
    with_link(|| {
        let store = new_store();
        let (sid, _ctx) = registered(&store);
        let generation = next_bridge_generation();
        attach_up(&store, generation);
        let hex = "aa".repeat(16);
        let held = format!("{hex}.1");
        let tell = |line: &str| cmd_link(&store, &format!("log {line}"), Some(generation));
        let at_seven = || deliver(&store, &sid, 7, "s-peer@n-lab", "note", "seven");
        assert_eq!(tell(&held), "OK\n");
        assert_eq!(at_seven(), "OK 1\n");
        let as_text_another_log = |told: &str| told != held;
        for same in [format!("{hex}.01"), format!("{hex}.{}1", "0".repeat(24))] {
            assert!(as_text_another_log(&same), "NEGATIVE CONTROL: {same}");
            assert_eq!(tell(&same), "OK\n", "{same} is the clock {held}");
            assert_eq!(at_seven(), "OK 1\n", "nothing forgotten by {same}");
        }
        assert_eq!(
            tell(&format!("{hex}.2 continues=20 was={hex}.001")),
            "OK forgot=1\n"
        );
        assert_eq!(
            at_seven(),
            "OK 1\n",
            "7 is below the fork of the move from {held}"
        );
    });
}

/// A seamless update that carried its clock with a zero-padded generation lands on the
/// clock its successor's bridge names canonically: the owed receipt and the queued reply
/// below are kept, and the successor carries the clock on in the canonical form. The same
/// carry naming another log forgets both, which is what the observation can tell.
///
/// NEGATIVE CONTROL: the carried text differs from the told identity, so an endpoint that
/// compared clocks as strings forgot the padded carry as it forgets another log's.
#[test]
fn a_carry_naming_a_padded_generation_keeps_what_the_clock_names() {
    let hex = "aa".repeat(16);
    let told = format!("{hex}.1");
    let outgoing = with_link(|| {
        let store = new_store();
        let (sid, ctx) = registered(&store);
        let generation = next_bridge_generation();
        attach_up(&store, generation);
        assert_eq!(
            cmd_link(&store, &format!("log {told}"), Some(generation)),
            "OK\n"
        );
        assert_eq!(
            deliver(&store, &sid, 7, "s-peer@n-lab", "ask", "ask"),
            "OK 1\n"
        );
        assert_eq!(cmd_inbox_seen(&ctx, "1 handled"), "OK seen=1\n");
        assert_eq!(
            cmd_post(&ctx, "to=@s-peer kind=answer re=7 reply-7", None),
            "OK 1\n"
        );
        aterm_toml::to_string(&ctx.fabric.handoff_carry(&mut CARRY_MAIL_BYTES.clone()))
            .expect("outgoing carry")
    });
    let line = format!("bus_log = \"{told}\"");
    assert!(outgoing.contains(&line), "{outgoing}");
    for (carried, kept) in [
        (format!("{hex}.01"), true),
        (format!("{}.1", "bb".repeat(16)), false),
    ] {
        if kept {
            assert_ne!(carried, told, "NEGATIVE CONTROL: as text, another log");
        }
        let source = outgoing.replace(&line, &format!("bus_log = \"{carried}\""));
        with_link(|| {
            let store = new_store();
            registered(&store);
            let generation = next_bridge_generation();
            attach_up(&store, generation);
            assert_eq!(
                cmd_link(&store, &format!("log {told}"), Some(generation)),
                "OK\n"
            );
            let late = test_handle(2);
            let ctx = late.ctx.clone();
            let incoming: FabricCarry = aterm_toml::from_str(&source).expect("incoming carry");
            ctx.fabric.seed_carry(&incoming, &ctx.timeline);
            register_on_log(&ctx, || {
                store
                    .write()
                    .unwrap_or_else(|p| p.into_inner())
                    .register(late);
            });
            let outbox = cmd_outbox(&store, "");
            assert_eq!(
                outbox.contains(" rid=1 off=7 verdict=handled"),
                kept,
                "{carried}: {outbox}"
            );
            assert_eq!(outbox.contains("reply-7"), kept, "{carried}: {outbox}");
            let onward = ctx.fabric.handoff_carry(&mut CARRY_MAIL_BYTES.clone());
            assert_eq!(onward.bus_log.as_deref(), Some(told.as_str()), "{carried}");
        });
    }
}

/// `link log` reads an identity, and a `was=`, exactly as astream-wire's
/// `LogIdentity::parse` does: a token is usage iff `parse` refuses it, and a `was=` is
/// taken iff it parses to an earlier generation of the told log
/// (`LogIdentity::continues_from`). The sweep holds canonical and zero-padded generations
/// (past 20 digits included), `u64::MAX` and one past it, and the malformed shapes.
///
/// NEGATIVE CONTROL: a grammar that bounds the generation to 20 digits, as the endpoint's
/// own copy did, disagrees on the sweep.
#[test]
fn link_log_reads_an_identity_exactly_as_astream_parses_it() {
    let hex = "aa".repeat(16);
    let tokens: Vec<String> = vec![
        format!("{hex}.0"),
        format!("{hex}.1"),
        format!("{hex}.01"),
        format!("{hex}.{}1", "0".repeat(24)),
        format!("{hex}.{}", u64::MAX - 1),
        format!("{hex}.{}", u64::MAX),
        format!("{hex}.0{}", u64::MAX),
        format!("{hex}.18446744073709551616"),
        format!("{}.1", "bb".repeat(16)),
        format!("{}.1", "AA".repeat(16)),
        format!("{}.1", "aa".repeat(15)),
        format!("{}.1", "aa".repeat(17)),
        format!("{}g.1", "a".repeat(31)),
        format!("{hex}."),
        hex.clone(),
        format!("{hex}.+1"),
        format!("{hex}.-1"),
        format!("{hex}.1.2"),
        format!("{hex}.x"),
        "nope".to_string(),
    ];
    let generation_digits = |t: &str| t.split_once('.').map_or(0, |(_, g)| g.len());
    assert!(
        tokens
            .iter()
            .any(|t| LogIdentity::parse(t).is_some() && generation_digits(t) > 20),
        "the sweep holds a padded generation past 20 digits"
    );
    let bounded = |t: &str| LogIdentity::parse(t).is_some() && generation_digits(t) <= 20;
    assert!(
        tokens
            .iter()
            .any(|t| bounded(t) != LogIdentity::parse(t).is_some()),
        "NEGATIVE CONTROL: a 20-digit bound refuses what astream accepts"
    );
    with_link(|| {
        let store = new_store();
        registered(&store);
        let generation = next_bridge_generation();
        attach_up(&store, generation);
        let tell = |line: &str| cmd_link(&store, &format!("log {line}"), Some(generation));
        for t in &tokens {
            assert_eq!(
                tell(t) != LINK_USAGE,
                LogIdentity::parse(t).is_some(),
                "log {t}"
            );
        }
        let top = format!("{hex}.{}", u64::MAX);
        let ident = LogIdentity::parse(&top).expect("the top generation");
        for t in &tokens {
            let line = format!("{top} continues=5 was={t}");
            let taken = LogIdentity::parse(t).is_some_and(|was| ident.continues_from(was));
            assert_eq!(tell(&line) != LINK_USAGE, taken, "{line}");
        }
    });
}

#[test]
fn a_session_carried_on_the_continued_generation_keeps_what_is_below_the_fork() {
    let a = |g: u64| format!("{}.{g}", "aa".repeat(16));
    let (a0, a2) = (a(0), a(2));
    let b0 = format!("{}.0", "bb".repeat(16));
    let carry = |on: &str| {
        with_link(|| {
            let store = new_store();
            let (sid, ctx) = registered(&store);
            let generation = next_bridge_generation();
            attach_up(&store, generation);
            assert_eq!(
                cmd_link(&store, &format!("log {on}"), Some(generation)),
                "OK\n"
            );
            for (id, off) in [(1, 7), (2, 40)] {
                assert_eq!(
                    deliver(&store, &sid, off, "s-peer@n-lab", "ask", "ask"),
                    format!("OK {id}\n")
                );
                assert_eq!(
                    cmd_inbox_seen(&ctx, &format!("{id} handled")),
                    format!("OK seen={id}\n")
                );
                assert_eq!(
                    cmd_post(
                        &ctx,
                        &format!("to=@s-peer kind=answer re={off} reply-{off}"),
                        None
                    ),
                    format!("OK {id}\n")
                );
            }
            ctx.fabric.handoff_carry(&mut CARRY_MAIL_BYTES.clone())
        })
    };
    let on_a0 = aterm_toml::to_string(&carry(&a0)).expect("outgoing carry");
    let on_a1 = aterm_toml::to_string(&carry(&a(1))).expect("outgoing carry");
    let on_a3 = aterm_toml::to_string(&carry(&a(3))).expect("outgoing carry");
    let on_b0 = aterm_toml::to_string(&carry(&b0)).expect("outgoing carry");
    let legacy = on_a0
        .lines()
        .filter(|line| !line.starts_with("bus_log ="))
        .collect::<Vec<_>>()
        .join("\n");
    for (name, source, kept) in [
        ("carried on A.0", on_a0.as_str(), true),
        ("carried on A.1, between", on_a1.as_str(), true),
        ("carried on A.3, past the clock", on_a3.as_str(), false),
        ("carried on B.0", on_b0.as_str(), false),
        ("carried with no clock", legacy.as_str(), false),
    ] {
        with_link(|| {
            let store = new_store();
            registered(&store);
            let generation = next_bridge_generation();
            attach_up(&store, generation);
            assert_eq!(
                cmd_link(&store, &format!("log {a0}"), Some(generation)),
                "OK\n"
            );
            assert_eq!(
                cmd_link(
                    &store,
                    &format!("log {a2} continues=20 was={a0}"),
                    Some(generation)
                ),
                "OK forgot=1\n"
            );
            assert_eq!(
                cmd_link(&store, &format!("log {a2}"), Some(generation)),
                "OK\n"
            );
            let late = test_handle(2);
            let ctx = late.ctx.clone();
            let incoming: FabricCarry = aterm_toml::from_str(source).expect("incoming carry");
            ctx.fabric.seed_carry(&incoming, &ctx.timeline);
            register_on_log(&ctx, || {
                store
                    .write()
                    .unwrap_or_else(|p| p.into_inner())
                    .register(late);
            });
            let outbox = cmd_outbox(&store, "");
            assert_eq!(
                outbox.contains(" rid=1 off=7 verdict=handled"),
                kept,
                "{name}: {outbox}"
            );
            assert!(!outbox.contains(" rid=2 "), "{name}: {outbox}");
            assert_eq!(outbox.contains("reply-7"), kept, "{name}: {outbox}");
            assert!(!outbox.contains("reply-40"), "{name}: {outbox}");
            assert_eq!(
                wait_landing(&ctx, 2, std::time::Instant::now()).err(),
                Some("ERR log-replaced id=2\n".to_string()),
                "{name}"
            );
        });
    }
}

#[test]
fn a_verdict_given_after_the_move_owes_its_receipt_below_the_fork() {
    let a0 = format!("{}.0", "aa".repeat(16));
    let a1 = format!("{}.1", "aa".repeat(16));
    for lineage in [true, false] {
        with_link(|| {
            let store = new_store();
            let (sid, ctx) = registered(&store);
            let generation = next_bridge_generation();
            attach_up(&store, generation);
            let case = format!("lineage={lineage}");
            let tell = |line: &str| cmd_link(&store, &format!("log {line}"), Some(generation));
            assert_eq!(tell(&a0), "OK\n", "{case}");
            assert_eq!(
                deliver(&store, &sid, 3, "s-peer@n-lab", "note", "read"),
                "OK 1\n"
            );
            assert_eq!(
                deliver(&store, &sid, 7, "s-peer@n-lab", "ask", "below"),
                "OK 2\n"
            );
            assert_eq!(
                deliver(&store, &sid, 40, "s-peer@n-lab", "ask", "above"),
                "OK 3\n"
            );
            assert_eq!(cmd_inbox_seen(&ctx, "1"), "OK seen=1\n");
            assert_eq!(last_inbox_seen(&ctx), "1 off=3 floor=7 seen=1", "{case}");

            let moved = if lineage {
                format!("{a1} continues=20 was={a0}")
            } else {
                a1.clone()
            };
            assert_eq!(tell(&moved), "OK forgot=1\n", "{case}");

            assert_eq!(cmd_inbox_seen(&ctx, "1"), "OK seen=1\n");
            assert_eq!(
                last_inbox_seen(&ctx),
                if lineage {
                    "1 off=3 floor=7 seen=1"
                } else {
                    "1 off=3 floor=- seen=1"
                },
                "{case}: the unread ask below the fork is still the floor"
            );
            assert_eq!(cmd_inbox_seen(&ctx, "2 handled"), "OK seen=2\n");
            assert_eq!(cmd_inbox_seen(&ctx, "3 handled"), "OK seen=3\n");
            let outbox = cmd_outbox(&store, "");
            assert_eq!(
                outbox.contains(&format!("receipt sid={sid} rid=1 off=7 verdict=handled")),
                lineage,
                "{case}: {outbox}"
            );
            assert!(!outbox.contains(" rid=2 "), "{case}: {outbox}");
            let earlier = cmd_outbox(&store, "earlier-receipts=1");
            assert!(
                earlier.contains(
                    " rid=2 off=40 verdict=handled kind=ask from=s-peer@n-lab log=earlier\n"
                ),
                "{case}: {earlier}"
            );
            assert_eq!(
                earlier.contains(
                    " rid=1 off=7 verdict=handled kind=ask from=s-peer@n-lab log=earlier\n"
                ),
                !lineage,
                "{case}: {earlier}"
            );
        });
    }
}

fn armed_watches(ctx: &SessionCtx) -> usize {
    ctx.fabric
        .lock()
        .watches
        .iter()
        .filter(|w| w.strong_count() > 0)
        .count()
}

#[test]
fn await_inbox_re_follows_what_the_move_kept() {
    let a0 = format!("{}.0", "aa".repeat(16));
    let a1 = format!("{}.1", "aa".repeat(16));
    let hang_ms =
        u64::try_from(aterm_spec::wait::MUST_HAPPEN.as_millis()).expect("a minute in milliseconds");
    for lineage in [true, false] {
        with_link(|| {
            let store = new_store();
            let (sid, ctx) = registered(&store);
            let generation = next_bridge_generation();
            attach_up(&store, generation);
            let case = format!("lineage={lineage}");
            let tell = |line: &str| cmd_link(&store, &format!("log {line}"), Some(generation));
            let park = |args: &'static [&'static str]| {
                let ctx = ctx.clone();
                std::thread::spawn(move || cmd_await_inbox(&ctx, args, hang_ms))
            };
            assert_eq!(tell(&a0), "OK\n", "{case}");
            for (id, off) in [(1, 9), (2, 41)] {
                assert_eq!(
                    cmd_post(&ctx, &format!("to=@s-peer kind=note at-{off}"), None),
                    format!("OK {id}\n")
                );
                assert_eq!(
                    cmd_deliver(&store, &format!("{sid} landed={id} off={off}")),
                    "OK\n"
                );
            }
            assert_eq!(
                cmd_deliver(
                    &store,
                    &format!("{sid} off=15 from=s-peer@n-lab kind=answer trust=agent re=9 text=r")
                ),
                "OK 1\n"
            );
            let on_nine = park(&["re=9", "since=1"]);
            let on_forty_one = park(&["re=41"]);
            let deadline = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
            while armed_watches(&ctx) < 2 {
                assert!(
                    std::time::Instant::now() < deadline,
                    "{case}: the two waits did not arm within a minute"
                );
                aterm_uds::deadline::poll_yield();
            }

            let moved = if lineage {
                format!("{a1} continues=20 was={a0}")
            } else {
                a1.clone()
            };
            assert_eq!(tell(&moved), "OK forgot=1\n", "{case}");
            assert_eq!(
                on_forty_one.join().expect("the wait finished"),
                "ERR log-replaced re=41\n",
                "{case}"
            );
            assert_eq!(
                cmd_await_inbox(&ctx, &["re=9", "since=0"], 0),
                if lineage {
                    "OK inbox 1\n"
                } else {
                    "OK timeout\n"
                },
                "{case}: the reply delivered on A.0"
            );
            if lineage {
                assert_eq!(
                    cmd_deliver(
                        &store,
                        &format!(
                            "{sid} off=21 from=s-peer@n-lab kind=ack trust=agent re=9 \
                             verdict=handled text=ack"
                        )
                    ),
                    "OK 2\n"
                );
            }
            assert_eq!(
                on_nine.join().expect("the wait finished"),
                if lineage {
                    "OK inbox 2\n"
                } else {
                    "ERR log-replaced re=9\n"
                },
                "{case}"
            );
        });
    }
}

#[test]
fn a_chain_of_moves_keeps_what_is_below_every_fork_however_long() {
    let a = |g: u64| format!("{}.{g}", "aa".repeat(16));
    for lineage in [true, false] {
        with_link(|| {
            let store = new_store();
            let (sid, ctx) = registered(&store);
            let generation = next_bridge_generation();
            attach_up(&store, generation);
            let case = format!("lineage={lineage}");
            let tell = |line: String| cmd_link(&store, &format!("log {line}"), Some(generation));
            assert_eq!(tell(a(0)), "OK\n", "{case}");
            assert_eq!(
                deliver(&store, &sid, 7, "s-peer@n-lab", "ask", "seven"),
                "OK 1\n"
            );
            for g in 1..=9 {
                let line = if lineage {
                    format!("{} continues={} was={}", a(g), 20 + g, a(g - 1))
                } else {
                    a(g)
                };
                assert_eq!(tell(line), "OK forgot=1\n", "{case}: move {g}");
            }
            assert_eq!(cmd_inbox_seen(&ctx, "1 handled"), "OK seen=1\n");
            assert_eq!(
                cmd_outbox(&store, "")
                    .contains(&format!("receipt sid={sid} rid=1 off=7 verdict=handled")),
                lineage,
                "{case}"
            );
            assert_eq!(
                cmd_outbox(&store, "earlier-receipts=1").contains(
                    " rid=1 off=7 verdict=handled kind=ask from=s-peer@n-lab log=earlier\n"
                ),
                !lineage,
                "{case}"
            );
            assert_eq!(
                deliver(&store, &sid, 7, "s-peer@n-lab", "ask", "seven"),
                if lineage { "OK 1\n" } else { "OK 2\n" },
                "{case}"
            );
            if lineage {
                assert_eq!(
                    cmd_inbox_get(&ctx, "@7"),
                    "OK 5 off=7 from=s-peer@n-lab kind=ask trust=agent\nseven",
                    "{case}"
                );
            }
        });
    }
    with_link(|| {
        let store = new_store();
        let (sid, ctx) = registered(&store);
        let generation = next_bridge_generation();
        attach_up(&store, generation);
        let tell = |line: String| cmd_link(&store, &format!("log {line}"), Some(generation));
        assert_eq!(tell(format!("{}.0", "bb".repeat(16))), "OK\n");
        assert_eq!(
            deliver(&store, &sid, 5, "s-peer@n-lab", "note", "b0"),
            "OK 1\n"
        );
        assert_eq!(tell(a(0)), "OK forgot=1\n", "another log");
        for g in 1..=9 {
            assert_eq!(
                tell(format!("{} continues=100 was={}", a(g), a(g - 1))),
                "OK forgot=1\n",
                "move {g}"
            );
        }
        assert_eq!(
            deliver(&store, &sid, 5, "s-peer@n-lab", "note", "a9"),
            "OK 2\n",
            "A.9's record at 5 is not B.0's"
        );
        assert_eq!(
            cmd_inbox_get(&ctx, "@5"),
            "OK 2 off=5 from=s-peer@n-lab kind=note trust=agent\na9"
        );
    });
}

fn parse_outbox(reply: &str) -> (usize, Vec<(String, String)>) {
    let (declared, posts, _) = parse_outbox_frame(reply);
    (declared, posts)
}

fn says_more(reply: &str) -> bool {
    parse_outbox_frame(reply).2
}

fn parse_outbox_frame(reply: &str) -> (usize, Vec<(String, String)>, bool) {
    let (head, payload) = reply.split_once('\n').expect("a header line");
    let declared: usize = head
        .strip_prefix("OK ")
        .and_then(|rest| rest.split(' ').next())
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("bad outbox header {head:?}"));
    assert_eq!(
        declared,
        payload.len(),
        "the length prefix must equal the bytes that follow"
    );
    let mut out = Vec::new();
    let mut more = false;
    let mut rest = payload;
    while !rest.is_empty() {
        let (line, tail) = rest.split_once('\n').expect("a post line");
        if line.starts_with("receipt ") || line.starts_with("fetch ") {
            rest = tail;
            continue;
        }
        if line == "more" {
            assert!(tail.is_empty(), "`more` is the frame's last line");
            more = true;
            rest = tail;
            continue;
        }
        let len: usize = line
            .rsplit_once(" len=")
            .and_then(|(_, n)| n.parse().ok())
            .unwrap_or_else(|| panic!("no len= on {line:?}"));
        assert!(tail.len() >= len, "the frame promised {len} body bytes");
        out.push((line.to_string(), tail[..len].to_string()));
        rest = &tail[len..];
    }
    (declared, out, more)
}

/// A post the bridge retires undelivered keeps the bridge's word for its waiting client and
/// says the [`DeadReason`] on its timeline row and fact; a landing says none, and a word this
/// build does not know, or one only aterm retires with, reads `undeliverable` (session log
/// Phase 6 fix unit F6, LOOP-5).
#[test]
fn a_retired_posts_row_says_the_reason_the_bridge_gave() {
    let store = new_store();
    let (sid, ctx) = registered(&store);
    let mut cases: Vec<(String, DeadReason)> = DeadReason::ALL
        .iter()
        .filter(|r| !r.aterms_own())
        .map(|r| (format!(" reason={}", r.word()), *r))
        .collect();
    cases.extend([
        (String::new(), DeadReason::Undeliverable),
        (" reason=caf".to_string(), DeadReason::Undeliverable),
        (" reason=not-carried".to_string(), DeadReason::Undeliverable),
    ]);
    for (id, (reason, why)) in (1u64..).zip(&cases) {
        assert_eq!(
            cmd_post(&ctx, "to=@s-abc@n-nowhere kind=note x", None),
            format!("OK {id}\n")
        );
        assert_eq!(
            cmd_outbox_sent(&store, &format!("{sid} {id} off=-{reason}")),
            "OK\n"
        );
        let told = reason.strip_prefix(" reason=").unwrap_or(DEAD_DEFAULT);
        let inbox = ctx.fabric.lock();
        let row = inbox.posts.iter().find(|p| p.id == id).expect("kept");
        assert_eq!(
            row.dead_reason, told,
            "the client is told the bridge's word"
        );
        drop(inbox);
        let tl = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
        assert_eq!(
            tl.facts.last().map(String::as_str),
            Some(
                format!("post-landed: MailLanded {{ post: {id}, off: None, why: Some({why:?}) }}")
                    .as_str()
            )
        );
        let last = tl.since(None).next_back().expect("a row");
        assert_eq!(
            (last.kind, last.fact.as_str()),
            (
                "post-landed",
                format!("{id} off=- reason={}", why.word()).as_str()
            )
        );
    }
    let id = cases.len() + 1;
    assert_eq!(
        cmd_post(&ctx, "to=@s-abc kind=note y", None),
        format!("OK {id}\n")
    );
    assert_eq!(
        cmd_outbox_sent(&store, &format!("{sid} {id} off=90")),
        "OK\n"
    );
    let tl = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
    let last = tl.since(None).next_back().expect("a row");
    assert_eq!(
        (last.kind, last.fact.as_str()),
        ("post-landed", format!("{id} off=90").as_str()),
        "a landing gives no reason"
    );
}

#[test]
fn outbox_hands_the_bridge_the_body_and_outbox_sent_retires_it() {
    let store = new_store();
    let (sid, ctx) = registered(&store);
    assert_eq!(
        cmd_post(&ctx, "to=@s-abc kind=note first body", None),
        "OK 1\n"
    );
    assert_eq!(
        cmd_post(
            &ctx,
            "to=h-andrew kind=answer re=90312 dl=240000 via=s-inner second",
            None
        ),
        "OK 2\n"
    );
    let (_, posts) = parse_outbox(&cmd_outbox(&store, ""));
    assert_eq!(posts.len(), 2, "both queued posts drain");
    assert!(
        posts[0]
            .0
            .starts_with(&format!("post sid={sid} id=1 to=@s-abc kind=note len=")),
        "{:?}",
        posts[0].0
    );
    assert_eq!(posts[0].1, "first body");
    assert_eq!(posts[1].1, "second");
    for field in ["kind=answer", "re=90312", "dl=240000", "via=s-inner"] {
        assert!(posts[1].0.contains(field), "{} lacks {field}", posts[1].0);
    }

    let (_, again) = parse_outbox(&cmd_outbox(&store, ""));
    assert_eq!(again, posts, "outbox moves nothing");

    assert_eq!(
        cmd_outbox_sent(&store, &format!("{sid} 1 off=90355")),
        "OK\n"
    );
    let (_, after) = parse_outbox(&cmd_outbox(&store, ""));
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].1, "second");
    {
        let inbox = ctx.fabric.lock();
        let row = inbox.posts.iter().find(|p| p.id == 1).expect("post 1 kept");
        assert_eq!(row.off, Some(90355));
        assert!(
            row.body.is_empty(),
            "the retained body is dropped at retirement"
        );
    }
    let listing = cmd_inbox(&ctx, "--peek");
    assert!(listing.contains("post 2 to=h-andrew"), "{listing}");
    assert!(
        !listing.contains("post 1 "),
        "a landed post is not in flight: {listing}"
    );

    assert_eq!(
        cmd_outbox_sent(&store, &format!("{sid} 1 off=90355")),
        "OK\n"
    );
    assert_eq!(cmd_outbox_sent(&store, &format!("{sid} 2 off=-")), "OK\n");
    let (n, none_left) = parse_outbox(&cmd_outbox(&store, ""));
    assert_eq!(
        (n, none_left.len()),
        (0, 0),
        "an undeliverable post leaves the queue"
    );
    assert_eq!(
        cmd_outbox_sent(&store, &format!("{sid} 9 off=1")),
        "ERR no such post\n"
    );
    assert_eq!(
        cmd_outbox_sent(&store, "s-nope 1 off=1"),
        "ERR no such session\n"
    );
}

#[test]
fn an_outbound_body_may_contain_newlines() {
    let store = new_store();
    let (_sid, ctx) = registered(&store);
    let body = "line one\nline two\n\nlen=7 post sid=fake id=99 to=x kind=note len=3\n";
    assert_eq!(
        cmd_post(
            &ctx,
            &format!("to=@s-abc kind=note len={}", body.len()),
            Some(body.as_bytes().to_vec())
        ),
        "OK 1\n"
    );
    let (_, posts) = parse_outbox(&cmd_outbox(&store, ""));
    assert_eq!(
        posts.len(),
        1,
        "the embedded post-shaped line is BODY, not a row"
    );
    assert_eq!(posts[0].1, body);
}

#[test]
fn a_full_outbox_refuses_the_post_instead_of_dropping_one() {
    let store = new_store();
    let (_sid, ctx) = registered(&store);
    for i in 1..=OUTBOX_CAP {
        assert_eq!(
            cmd_post(&ctx, &format!("to=@s-abc kind=note m{i}"), None),
            format!("OK {i}\n")
        );
    }
    let refused = cmd_post(&ctx, "to=@s-abc kind=note one-too-many", None);
    assert!(
        refused.starts_with(&format!("ERR outbox full queued={OUTBOX_CAP} ")),
        "{refused}"
    );
    let (_, posts) = parse_outbox(&cmd_outbox(&store, ""));
    assert_eq!(
        posts.len(),
        OUTBOX_CAP,
        "nothing already queued was evicted"
    );
    assert_eq!(posts[0].1, "m1", "the OLDEST message is still there");
}

#[test]
fn a_parked_post_wakes_on_an_undeliverable_verdict() {
    with_link(|| {
        stale_after_a_hang();
        let store = new_store();
        let (sid, ctx) = registered(&store);
        attach_up(&store, next_bridge_generation());
        let waiter = {
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                cmd_post(&ctx, "to=@s-abc kind=ask --wait=60000 hello", None)
            })
        };
        let deadline = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
        while !waiter.is_finished() {
            let reply = cmd_outbox_sent(&store, &format!("{sid} 1 off=-"));
            assert!(reply == "OK\n" || reply == "ERR no such post\n", "{reply}");
            assert!(
                std::time::Instant::now() < deadline,
                "the waiter never woke"
            );
            aterm_uds::deadline::poll_yield();
        }
        assert_eq!(
            waiter.join().expect("the waiter thread"),
            "ERR undeliverable id=1\n"
        );
    });
}

#[test]
fn outbox_drains_every_session_and_max_bounds_the_batch() {
    let store = new_store();
    let (sid_a, ctx_a) = registered(&store);
    let (sid_b, ctx_b) = registered_as(&store, 2);
    assert_eq!(cmd_post(&ctx_a, "to=h-x kind=note from-a", None), "OK 1\n");
    assert_eq!(cmd_post(&ctx_b, "to=h-x kind=note from-b", None), "OK 1\n");
    let whole = cmd_outbox(&store, "");
    assert!(!says_more(&whole), "every post listed: {whole:?}");
    let (_, posts) = parse_outbox(&whole);
    assert_eq!(posts.len(), 2);
    assert!(
        posts[0].0.contains(&format!("sid={sid_a}")),
        "{:?}",
        posts[0].0
    );
    assert!(
        posts[1].0.contains(&format!("sid={sid_b}")),
        "{:?}",
        posts[1].0
    );
    let cut = cmd_outbox(&store, "1");
    assert!(says_more(&cut), "B's post is unlisted: {cut:?}");
    let (_, one) = parse_outbox(&cut);
    assert_eq!(one.len(), 1, "`max` bounds the batch");
    assert_eq!(one[0].1, "from-a");
    assert_eq!(cmd_outbox(&store, "nope"), OUTBOX_USAGE);
}

#[test]
fn a_parked_post_learns_the_bridge_died_rather_than_timing_out() {
    with_link(|| {
        stale_after_a_hang();
        let store = new_store();
        let (_sid, ctx) = registered(&store);
        let generation = next_bridge_generation();
        attach_up(&store, generation);

        let poster = {
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                cmd_post(
                    &ctx,
                    "to=@s-peer kind=ask --wait=600000 which branch?",
                    None,
                )
            })
        };
        let deadline = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
        while !cmd_inbox(&ctx, "--peek").contains("post 1 ") {
            assert!(
                std::time::Instant::now() < deadline,
                "the post never reached the outbox"
            );
            aterm_uds::deadline::poll_yield();
        }
        bridge_lost(&store, generation);

        assert_eq!(
            poster.join().expect("the poster thread"),
            "ERR fabric disconnected id=1 queued=1\n",
            "a parked waiter must get the verdict, not its whole timeout"
        );
    });
}

#[test]
fn post_may_not_claim_a_verdict_a_bridge_records() {
    with_link(post_may_not_claim_a_verdict_a_bridge_records_body);
}

fn post_may_not_claim_a_verdict_a_bridge_records_body() {
    let store = new_store();
    let (sid, ctx) = registered(&store);

    for kind in ["expired", "undeliverable"] {
        assert_eq!(
            cmd_post(
                &ctx,
                &format!("to=@s-victim kind={kind} re=90312 nope"),
                None
            ),
            POST_USAGE,
            "`post kind={kind}` forges a bridge-recorded verdict"
        );
    }
    for (i, kind) in POSTABLE.iter().enumerate() {
        let id = i + 1;
        let reply = cmd_post(&ctx, &format!("to=@s-peer kind={kind} hi"), None);
        let expected = if matches!(*kind, "ask" | "task") {
            format!("ERR fabric absent id={id} no-bridge=1\n")
        } else {
            format!("OK {id}\n")
        };
        assert_eq!(reply, expected, "`post kind={kind}` is one of the seven");
    }

    for kind in ["expired", "undeliverable"] {
        assert_eq!(
            cmd_deliver(
                &store,
                &format!("{sid} off=1 from=h-a kind={kind} trust=agent text=x")
            ),
            "OK 1\n",
            "`deliver kind={kind}` is the bridge recording a verdict, and stays legal"
        );
    }
}

#[test]
fn eviction_takes_an_agent_row_before_a_listed_human_one_and_counts_it() {
    let store = new_store();
    let (sid, ctx) = registered(&store);

    assert_eq!(
        deliver(&store, &sid, 1, "h-andrew", "task", "stop"),
        "OK 1\n"
    );
    assert!(cmd_inbox(&ctx, "").contains("from=h-andrew"));
    assert!(
        ctx.fabric.lock().rows.iter().all(|r| r.listed),
        "the human's row is listed, which is what used to make it the victim"
    );

    for off in 2..=u64::try_from(RING_CAP).expect("cap fits") {
        assert!(deliver(&store, &sid, off, &format!("a-svc{off}"), "note", "x").starts_with("OK "));
    }
    assert_eq!(ctx.fabric.lock().rows.len(), RING_CAP);

    assert!(deliver(&store, &sid, 9999, "a-late", "note", "x").starts_with("OK "));
    let reply = cmd_inbox(&ctx, "--peek");
    assert!(
        reply.contains("from=h-andrew kind=task"),
        "a LISTED human row was evicted ahead of 511 unlisted agent rows"
    );
    assert!(!reply.contains("from=a-svc2 "), "the oldest agent row left");

    assert!(
        reply.contains(" dropped=1 "),
        "an evicted unhandled row must be reported: {}",
        reply.lines().next().unwrap_or("")
    );
}

#[test]
fn an_evicted_handled_row_is_not_reported_as_dropped() {
    let store = new_store();
    let (sid, ctx) = registered(&store);
    for off in 1..=u64::try_from(RING_CAP).expect("cap fits") {
        deliver(&store, &sid, off, &format!("a-svc{off}"), "note", "x");
    }
    cmd_inbox(&ctx, "");
    assert_eq!(cmd_inbox_seen(&ctx, "1 handled"), "OK seen=1\n");
    deliver(&store, &sid, 9999, "a-late", "note", "x");
    assert!(
        cmd_inbox(&ctx, "--peek").contains(" dropped=0 "),
        "row 1 was handled; losing it is not a drop the agent needs told about"
    );
}

#[test]
fn pending_counts_the_rows_a_bounded_or_filtered_reply_left_behind() {
    let store = new_store();
    let (sid, ctx) = registered(&store);
    for off in 1..=10 {
        deliver(&store, &sid, off, "a-svc", "note", "x");
    }
    let header = |reply: &str| reply.lines().next().unwrap_or("").to_string();

    let filtered = header(&cmd_inbox(&ctx, "since=5"));
    assert!(filtered.starts_with("OK 5 "), "{filtered}");
    assert!(
        filtered.contains(" pending=5"),
        "five rows below the newest listed one are still unread: {filtered}"
    );

    let bounded = header(&cmd_inbox(&ctx, "1"));
    assert!(bounded.starts_with("OK 1 "), "{bounded}");
    assert!(
        bounded.contains(" pending=5"),
        "the reply carried one row and left five: {bounded}"
    );

    assert!(header(&cmd_inbox(&ctx, "")).contains(" pending=0"));
}

#[test]
fn a_late_attach_from_a_dead_launch_cannot_regress_the_live_generation() {
    with_link(|| {
        let store = new_store();
        let (sid, _ctx) = registered(&store);

        let first = next_bridge_generation();
        bridge_attached(first);
        assert_eq!(deliver(&store, &sid, 1, "h-a", "task", "go"), "OK 1\n");

        let second = next_bridge_generation();
        attach_up(&store, second);
        assert_eq!(fabric_state(), "connected");

        bridge_attached(first);
        assert_eq!(
            fabric_state(),
            "connected",
            "a ghost lane's attach must not regress the owning generation"
        );

        assert_eq!(
            bridge_lost(&store, first),
            1,
            "the governed set is counted, whichever generation's loss it is"
        );
        assert_eq!(
            fabric_state(),
            "connected",
            "the ghost lane pinned the LIVE bridge at disconnected"
        );

        assert_eq!(bridge_lost(&store, second), 1);
        assert_eq!(fabric_state(), "disconnected");
    });
}

/// A DEAD GENERATION'S LATE LOSS HOLDS AND ADVISES NO SESSION: a newer bridge owns the link
/// and serves the sessions the dead one governed, and may already have freed them. The
/// owner's own loss still holds them, and so does the loss of a generation newer than the
/// owner (one that never attached). However many say one generation's loss, it advises a
/// session once.
///
/// NEGATIVE CONTROL: without the `superseded` check on `HoldAct::Lost` in [`apply_hold_as`],
/// the first generation's late loss holds the session the second one freed (2026-11-05).
#[test]
fn a_dead_generations_late_loss_holds_and_advises_no_session() {
    fn reason(ctx: &SessionCtx) -> Option<String> {
        ctx.fabric.hold().map(|hold| hold.reason)
    }
    with_link(|| {
        set_hold_on_bridge_lost(true);
        let store = new_store();
        let (sid, ctx) = registered(&store);
        let first = next_bridge_generation();
        bridge_attached(first);
        assert_eq!(bridge_hold(&store, &format!("{sid} off")), "OK hold=0\n");
        let second = next_bridge_generation();
        bridge_attached(second);
        assert_eq!(bridge_hold(&store, &format!("{sid} off")), "OK hold=0\n");

        assert_eq!(bridge_lost(&store, first), 1, "the governed set is counted");
        assert_eq!(
            reason(&ctx),
            None,
            "a dead generation's loss held the session"
        );

        assert_eq!(bridge_lost(&store, second), 1);
        assert_eq!(
            reason(&ctx).as_deref(),
            Some(FABRIC_LOST),
            "the owner's loss holds"
        );
        assert_eq!(bridge_hold(&store, &format!("{sid} off")), "OK hold=0\n");
        let never_attached = next_bridge_generation();
        assert_eq!(bridge_lost(&store, never_attached), 1);
        assert_eq!(
            reason(&ctx).as_deref(),
            Some(FABRIC_LOST),
            "a generation newer than the owner is no dead one"
        );
    });
    with_link(|| {
        let store = new_store();
        let (sid, ctx) = registered(&store);
        let first = next_bridge_generation();
        bridge_attached(first);
        assert_eq!(bridge_hold(&store, &format!("{sid} off")), "OK hold=0\n");
        let second = next_bridge_generation();
        bridge_attached(second);
        assert_eq!(bridge_lost(&store, first), 1);
        for _ in 0..3 {
            assert_eq!(bridge_lost(&store, second), 1);
        }
        let facts = ctx.timeline.lock().unwrap().facts.clone();
        let advised: Vec<_> = facts
            .iter()
            .filter(|f| f.starts_with("hold-advisory: "))
            .collect();
        assert_eq!(
            advised,
            [&format!(
                "hold-advisory: Advisory {{ origin: \"fleet\", reason: \"fabric-lost\", \
                 generation: {} }}",
                second.0
            )],
            "one advisory, of the owner's loss alone: {facts:?}"
        );
        assert_eq!(
            ctx.fabric
                .hold_advisory()
                .map(|hold| hold.reason)
                .as_deref(),
            Some(FABRIC_LOST)
        );
    });
}

/// A LOST GENERATION'S LANE HOLDS AND FREES NOTHING: a `hold` its lane had already read when
/// its supervisor said the loss lands after the exit sweep, or after the next attach, and
/// moves no slot. Ahead of the next attach it cannot lift the sweep's `fabric-lost`; past it
/// the lane's own late loss is a dead generation's, which holds nothing again. Its `hold on`
/// cannot halt a session the next bridge freed, under either setting of the knob.
///
/// NEGATIVE CONTROLS (2026-11-05): without either lane check, [`cmd_hold_within_waking`]'s or
/// [`apply_hold_as`]'s, the late `hold off` lifts the exit sweep's hold and nothing holds the
/// session again; without the one under the lock alone, the verb parked on the fence before
/// its generation's loss halts the session once the fence lifts.
#[test]
fn a_lost_generations_lane_holds_and_frees_nothing() {
    fn reason(ctx: &SessionCtx) -> Option<String> {
        ctx.fabric.hold().map(|hold| hold.reason)
    }
    with_link(|| {
        set_hold_on_bridge_lost(true);
        let store = new_store();
        let (sid, ctx) = registered(&store);
        let off = format!("{sid} off");
        let first = next_bridge_generation();
        bridge_attached(first);
        assert_eq!(on_lane(first, || bridge_hold(&store, &off)), "OK hold=0\n");
        assert_eq!(bridge_lost(&store, first), 1, "the supervisor's exit sweep");
        assert_eq!(reason(&ctx).as_deref(), Some(FABRIC_LOST));
        assert_eq!(
            on_lane(first, || bridge_hold(&store, &off)),
            HOLD_LANE_LOST,
            "the lost lane's last verb, landing after the exit sweep"
        );
        assert_eq!(
            reason(&ctx).as_deref(),
            Some(FABRIC_LOST),
            "a lost lane lifted the exit sweep's hold"
        );
        let second = next_bridge_generation();
        bridge_attached(second);
        assert_eq!(bridge_lost(&store, first), 1, "the lost lane's own guard");
        assert_eq!(
            reason(&ctx).as_deref(),
            Some(FABRIC_LOST),
            "the session runs unheld beside a bridge that has not freed it"
        );
        assert_eq!(on_lane(second, || bridge_hold(&store, &off)), "OK hold=0\n");
        assert_eq!(
            on_lane(first, || bridge_hold(
                &store,
                &format!("{sid} on reason=stop")
            )),
            HOLD_LANE_LOST
        );
        assert_eq!(reason(&ctx), None, "a dead bridge halted a session");
    });
    with_link(|| {
        let store = new_store();
        let (sid, ctx) = registered(&store);
        let first = next_bridge_generation();
        bridge_attached(first);
        let second = next_bridge_generation();
        bridge_attached(second);
        assert_eq!(
            on_lane(first, || bridge_hold(
                &store,
                &format!("{sid} on reason=stop")
            )),
            HOLD_LANE_LOST,
            "a superseded lane, its loss unsaid"
        );
        assert_eq!(reason(&ctx), None);
        assert!(
            !touched_contains(&sid),
            "a lost lane's refused hold governs nothing"
        );
        assert_eq!(
            on_lane(second, || bridge_hold(
                &store,
                &format!("{sid} on reason=stop")
            )),
            "OK hold=1\n",
            "the owner's lane holds"
        );
        assert_eq!(
            bridge_hold(&store, &format!("{sid} off")),
            "OK hold=0\n",
            "a hold no lane issued is judged by no lane's generation"
        );
    });
    // The verb in flight: read while its generation governed, parked on a committing
    // update's fence, then its generation's loss said before it takes the lock.
    with_link(|| {
        let store = new_store();
        let (sid, ctx) = registered(&store);
        let generation = next_bridge_generation();
        bridge_attached(generation);
        ctx.fabric.fence_hold(true);
        let before = ctx.fabric.fence_waits();
        let lane = {
            let store = Arc::clone(&store);
            let rest = format!("{sid} on reason=stop");
            std::thread::spawn(move || {
                on_lane(generation, || {
                    let wait = Some(aterm_spec::wait::MUST_HAPPEN);
                    cmd_hold_within(&store, &rest, HoldIssuer::Bridge, wait)
                })
            })
        };
        assert_parked_on_the_fence(
            &ctx.fabric,
            before,
            || lane.is_finished(),
            "a lane's hold, read before its bridge's loss",
        );
        assert_eq!(bridge_lost(&store, generation), 1);
        ctx.fabric.lift_hold_fence();
        assert_eq!(lane.join().expect("the lane's thread"), HOLD_LANE_LOST);
        assert_eq!(
            reason(&ctx),
            None,
            "a lost bridge's verb in flight halted a session"
        );
    });
}

#[test]
fn one_registrys_prune_does_not_evict_another_registrys_governed_sids() {
    with_link(|| {
        let bystander = new_store();
        let (b_sid, _b_ctx) = registered_as(&bystander, 1);
        assert_eq!(deliver(&bystander, &b_sid, 1, "h-a", "note", "x"), "OK 1\n");
        assert!(touched_contains(&b_sid), "the bystander is governed");

        let driver = new_store();
        let n = TOUCHED_PRUNE_AT + 8;
        let mut d_sids = Vec::with_capacity(n);
        for local in 1..=n as u64 {
            let (sid, _ctx) = registered_as(&driver, local);
            assert_eq!(deliver(&driver, &sid, local, "h-a", "note", "x"), "OK 1\n");
            d_sids.push(sid);
        }
        assert!(
            touched_contains(&b_sid),
            "a prune driven by one registry must not forget another's live session"
        );

        for local in 1..n as u64 {
            driver
                .write()
                .unwrap_or_else(|p| p.into_inner())
                .deregister_local(local);
        }
        let last = d_sids.last().expect("a session").clone();
        assert_eq!(
            deliver(&driver, &last, 9_999, "h-a", "note", "again"),
            "OK 2\n"
        );
        for sid in &d_sids[..n - 1] {
            assert!(
                !touched_contains(sid),
                "the driver's own dead sids are still pruned"
            );
        }
        assert!(
            touched_contains(&last),
            "the driver's live session survives"
        );
        assert!(
            touched_contains(&b_sid),
            "and the bystander is still governed after the driver's prune"
        );
    });
}

#[test]
fn one_peer_cannot_mint_extra_quota_by_rotating_the_sid_it_claims() {
    let store = new_store();
    let (sid, ctx) = registered(&store);
    assert_eq!(
        deliver(&store, &sid, 1, "h-andrew", "task", "stop"),
        "OK 1\n"
    );

    let mut accepted = 0usize;
    let mut refused = 0usize;
    for off in 2..=201u64 {
        let from = format!("s-rogue{off:04}@n-rogue");
        match deliver(&store, &sid, off, &from, "note", "spam").as_str() {
            "ERR quota\n" => refused += 1,
            ok if ok.starts_with("OK ") => accepted += 1,
            other => panic!("unexpected deliver reply {other:?}"),
        }
    }
    assert_eq!(
        accepted, SENDER_QUOTA,
        "one PEER gets one allowance however many sids it claims"
    );
    assert_eq!(refused, 200 - SENDER_QUOTA);

    assert!(
        deliver(&store, &sid, 900, "s-a@n-other", "note", "hello").starts_with("OK "),
        "an honest peer must not be charged for the rogue's flood"
    );
    assert!(deliver(&store, &sid, 901, "h-andrew", "note", "hi").starts_with("OK "));
    let reply = cmd_inbox(&ctx, "--peek");
    assert!(reply.contains("from=h-andrew kind=task"), "{reply}");
    assert!(reply.contains(" dropped=0 "), "{reply}");
    cmd_inbox(&ctx, "");
    assert!(deliver(&store, &sid, 902, "s-rogue9999@n-rogue", "note", "again").starts_with("OK "));
}

#[test]
fn an_ack_releases_the_senders_quota() {
    let store = new_store();
    let (sid, ctx) = registered(&store);
    for off in 1..=SENDER_QUOTA as u64 {
        assert!(deliver(&store, &sid, off, "s-p@n-peer", "note", "x").starts_with("OK "));
    }
    assert_eq!(
        deliver(&store, &sid, 100, "s-p@n-peer", "note", "x"),
        "ERR quota\n",
        "the quota is full"
    );
    cmd_inbox(&ctx, "--peek");
    assert_eq!(
        deliver(&store, &sid, 101, "s-p@n-peer", "note", "x"),
        "ERR quota\n",
        "`--peek` must not relieve the quota"
    );
    assert_eq!(
        cmd_inbox_seen(&ctx, &format!("{SENDER_QUOTA} handled")),
        format!("OK seen={SENDER_QUOTA}\n")
    );
    assert!(
        deliver(&store, &sid, 102, "s-p@n-peer", "note", "x").starts_with("OK "),
        "an acknowledged row must stop counting against its sender"
    );
    let _ = sid;
}

#[test]
fn a_bridge_truncated_body_says_so_in_the_row_and_in_inbox_get() {
    let store = new_store();
    let (sid, ctx) = registered(&store);
    assert_eq!(
        cmd_deliver(
            &store,
            &format!("{sid} off=1 from=s-a@n-b kind=report trust=agent len=200000 text=cut")
        ),
        "OK 1\n"
    );
    let row = rows(&cmd_inbox(&ctx, "--peek"))[0].to_string();
    assert!(row.contains(" len=200000"), "{row}");
    assert!(
        row.contains(" truncated=1"),
        "a cut row must say the endpoint never received the rest: {row}"
    );
    assert!(
        row.contains(" more=1"),
        "a cut row is never the whole of the message, however short: {row}"
    );
    assert_eq!(
        cmd_inbox_get(&ctx, "1"),
        "OK 3 truncated=1 len=200000\ncut",
        "`inbox get` must not answer a cut body under a complete body's header"
    );

    assert_eq!(
        cmd_deliver(
            &store,
            &format!("{sid} off=2 from=s-a@n-b kind=report trust=agent text=whole")
        ),
        "OK 2\n"
    );
    assert_eq!(cmd_inbox_get(&ctx, "2"), "OK 5\nwhole");
    let row2 = rows(&cmd_inbox(&ctx, "--peek"))[1].to_string();
    assert!(!row2.contains("truncated"), "{row2}");
}

#[test]
fn a_post_refused_for_want_of_a_link_says_it_is_still_queued() {
    with_link(|| {
        let store = new_store();
        let (_sid, ctx) = registered(&store);

        assert_eq!(
            cmd_post(&ctx, "to=@s-peer kind=ask --wait=0 no fabric?", None),
            "ERR fabric absent id=1 no-bridge=1\n"
        );

        note_bridge_supervised();
        assert_eq!(
            cmd_post(&ctx, "to=@s-peer kind=ask --wait=0 which branch?", None),
            "ERR fabric absent id=2 queued=1\n"
        );

        let drained = cmd_outbox(&store, "");
        assert!(drained.contains("no fabric?"), "{drained}");
        assert!(drained.contains("which branch?"), "{drained}");
    });
}

#[test]
fn one_outbox_drain_is_bounded_in_bytes_across_every_session() {
    let store = new_store();
    let body = vec![b'x'; BODY_MAX];
    for local in 1..=3u64 {
        let (_sid, ctx) = registered_as(&store, local);
        for _ in 0..(OUTBOX_BYTES_MAX / BODY_MAX) {
            assert!(
                cmd_post(&ctx, "to=@s-peer kind=note", Some(body.clone())).starts_with("OK "),
                "the per-session queue takes its own {OUTBOX_BYTES_MAX} bytes"
            );
        }
    }
    let drained = cmd_outbox(&store, "");
    assert!(
        drained.len() <= OUTBOX_DRAIN_BYTES_MAX + BODY_MAX + 256,
        "one drain answered {} bytes: the budget is the aggregate plus at most \
         the one post that crossed it",
        drained.len()
    );
    assert!(
        drained.len() > OUTBOX_DRAIN_BYTES_MAX / 2,
        "the budget must still hand the bridge a useful batch"
    );
    assert!(
        says_more(&drained),
        "a cut walk says so, last, so the bridge does not take it for the whole queue"
    );
    let again = cmd_outbox(&store, "");
    assert_eq!(
        again.len(),
        drained.len(),
        "a bounded drain retires nothing"
    );
}

#[test]
fn a_walk_stopped_at_the_budget_with_nothing_left_says_no_more() {
    let store = new_store();
    let body = vec![b'x'; BODY_MAX];
    let budget = OUTBOX_DRAIN_BYTES_MAX / BODY_MAX;
    let (_a, ctx_a) = registered_as(&store, 1);
    let (_b, ctx_b) = registered_as(&store, 2);
    for _ in 1..budget {
        assert!(cmd_post(&ctx_a, "to=@s-peer kind=note", Some(body.clone())).starts_with("OK "));
    }
    assert!(cmd_post(&ctx_b, "to=@s-peer kind=note", Some(body.clone())).starts_with("OK "));
    let whole = cmd_outbox(&store, "");
    let (_, posts) = parse_outbox(&whole);
    assert_eq!(posts.len(), budget);
    assert!(!says_more(&whole), "every post listed");
    assert!(cmd_post(&ctx_a, "to=h-x kind=note after", None).starts_with("OK "));
    let cut = cmd_outbox(&store, "");
    let (_, posts) = parse_outbox(&cut);
    assert_eq!(posts.len(), budget, "the budget binds before `after`");
    assert!(
        says_more(&cut),
        "the control: a post past the budget is unlisted"
    );
}

#[test]
fn a_full_queue_ahead_in_drain_order_does_not_hide_a_later_sessions_post() {
    let store = new_store();
    let body = vec![b'x'; BODY_MAX];
    let (sid_a, ctx_a) = registered_as(&store, 1);
    for _ in 0..(OUTBOX_BYTES_MAX / BODY_MAX) {
        assert!(
            cmd_post(&ctx_a, "to=@s-peer kind=note", Some(body.clone())).starts_with("OK "),
            "A's queue takes its own {OUTBOX_BYTES_MAX} bytes"
        );
    }
    let (sid_b, ctx_b) = registered_as(&store, 2);
    assert_eq!(cmd_post(&ctx_b, "to=h-x kind=note from-b", None), "OK 1\n");
    let (_, posts) = parse_outbox(&cmd_outbox(&store, ""));
    assert!(
        posts
            .iter()
            .any(|(line, body)| line.contains(&format!("sid={sid_b} ")) && body == "from-b"),
        "B's post is listed although A's queue alone fills the budget"
    );
    let ids: Vec<u64> = posts
        .iter()
        .filter(|(line, _)| line.contains(&format!("sid={sid_a} ")))
        .map(|(line, _)| {
            line.split_whitespace()
                .find_map(|t| t.strip_prefix("id="))
                .and_then(|n| n.parse().ok())
                .expect("id=")
        })
        .collect();
    assert!(!ids.is_empty(), "A's head post is listed too");
    assert!(
        ids.windows(2).all(|w| w[1] == w[0] + 1) && ids[0] == 1,
        "A's posts in its own queue order, from its head: {ids:?}"
    );
}

#[test]
fn repeated_outbox_peeks_do_not_starve_a_session_past_the_byte_budget() {
    let store = new_store();
    let body = vec![b'x'; BODY_MAX];
    let mut queued = Vec::new();
    for local in 1..=16 {
        let (sid, ctx) = registered_as(&store, local);
        assert_eq!(
            cmd_post(&ctx, "to=h-x kind=note", Some(body.clone())),
            "OK 1\n"
        );
        queued.push((sid, ctx));
    }
    let (last, ctx) = registered_as(&store, 17);
    assert_eq!(cmd_post(&ctx, "to=h-x kind=note allowed", None), "OK 1\n");
    queued.push((last.clone(), ctx));

    let mut exposed = BTreeSet::new();
    for _ in 0..2 {
        let drain = cmd_outbox(&store, "");
        assert!(drain.len() <= OUTBOX_DRAIN_BYTES_MAX + BODY_MAX + 4096);
        let (_, posts) = parse_outbox(&drain);
        for (line, _) in posts {
            let sid = line
                .split_whitespace()
                .find_map(|word| word.strip_prefix("sid="))
                .expect("post sid=");
            exposed.insert(sid.to_string());
        }
    }
    assert!(
        exposed.contains(&last.to_string()) && exposed.len() == queued.len(),
        "every sender is exposed although the earlier heads stay queued: {exposed:?}"
    );
    for (_, ctx) in queued {
        let inbox = ctx.fabric.lock();
        assert_eq!(inbox.posts.len(), 1, "a peek retires no post");
        assert_eq!(inbox.posts[0].id, 1);
        assert!(inbox.posts[0].off.is_none() && !inbox.posts[0].dead);
    }
}

#[test]
fn wire_parsers_reject_leading_plus_signs_on_unsigned_fields() {
    with_link(|| {
        let store = new_store();
        let (sid, ctx) = registered(&store);
        let generation = next_bridge_generation();
        attach_up(&store, generation);

        assert_eq!(cmd_hello("lane=+1 build=x", Some(generation)), HELLO_USAGE);
        for bad in [
            "set-aside files=+1",
            "quarantine torn=+1",
            "gap from=+1 to=2",
            "gap from=1 to=+2",
            "gap from=1 to=2 records=+1",
            "up rtt=+1",
            "log aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.1 continues=+1 was=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.0",
        ] {
            assert_eq!(cmd_link(&store, bad, Some(generation)), LINK_USAGE, "{bad}");
        }
        for bad in [
            format!("{sid} off=+1 from=h-a kind=note text=x"),
            format!("{sid} off=1 from=h-a kind=note re=+1 text=x"),
            format!("{sid} off=1 from=h-a kind=note dl=+1 text=x"),
            format!("{sid} landed=+1 off=1"),
            format!("{sid} landed=1 off=+1"),
            format!("{sid} receipt=+1 off=1"),
            format!("{sid} receipt=1 off=+1"),
            format!("{sid} fetched=+1 err=no-record"),
            format!("{sid} fetched=1 err=retained floor=+1"),
            format!("{sid} fetched=1 from=h-a kind=note at=+1 text=x"),
        ] {
            assert_eq!(cmd_deliver(&store, &bad), DELIVER_USAGE, "{bad}");
        }
        assert_eq!(
            cmd_deliver(
                &store,
                &format!("{sid} off=1 from=h-a kind=note len=+1 text=x")
            ),
            "ERR too large\n"
        );
        assert_eq!(cmd_outbox(&store, "+1"), OUTBOX_USAGE);
        assert_eq!(
            cmd_outbox_sent(&store, &format!("{sid} +1 off=1")),
            OUTBOX_USAGE
        );
        assert_eq!(
            cmd_outbox_sent(&store, &format!("{sid} 1 off=+1")),
            OUTBOX_USAGE
        );
        assert_eq!(cmd_inbox(&ctx, "+1"), INBOX_USAGE);
        assert_eq!(cmd_inbox(&ctx, "since=+1"), INBOX_USAGE);
        assert_eq!(cmd_inbox_get(&ctx, "+1"), INBOX_USAGE);
        assert_eq!(cmd_inbox_get(&ctx, "@+1"), INBOX_USAGE);
        assert_eq!(cmd_inbox_seen(&ctx, "+1"), INBOX_USAGE);
        assert_eq!(post_len("+1"), Err(POST_LEN_USAGE.to_string()));
        for bad in [
            "to=h-a kind=note dl=+1 hi",
            "to=h-a kind=ask --wait=+1 hi",
            "to=h-a kind=ask --wait --wait-ack=+1 hi",
        ] {
            assert_eq!(cmd_post(&ctx, bad, None), POST_USAGE, "{bad}");
        }
        for bad in [
            &["since=+1"][..],
            &["since=f+1"][..],
            &["since=0", "fleet_since=f+1"][..],
        ] {
            assert!(
                cmd_await_inbox(&ctx, bad, 0).starts_with("ERR usage"),
                "{bad:?}"
            );
        }
    });
}
