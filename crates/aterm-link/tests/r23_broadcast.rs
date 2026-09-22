// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **ROUND 23 — BROADCAST: one record on the log, however many sessions read
//! it, and nobody reads one they did not ask for.**
//!
//! §6 states the shape in one line — "derived from subscription, one log record
//! per broadcast" — and every test here is over the real binaries, because the
//! two halves that make the sentence true live in different processes: the
//! endpoint holds the opt-in (`topic add`) and the bridge holds the one
//! subscription that fans it in.

mod harness;

use harness::{until, World, FLEET};

/// Every record currently on the fleet's broadcast subtree.
fn say_records(w: &World) -> Vec<(u64, String, String)> {
    let filter = format!("/f/{FLEET}/pub/*/*/say/>");
    let mut c = w.god();
    let mut out = Vec::new();
    let mut from = 0u64;
    loop {
        let (rows, (next, head)) = c.fetch(from, &filter, 256).expect("fetch the say subtree");
        for (off, subject, body) in rows {
            out.push((off, subject, String::from_utf8_lossy(&body).into_owned()));
        }
        if next >= head {
            return out;
        }
        from = next.max(from + 1);
    }
}

/// Add a topic and wait for the bridge to have sampled it — the set lives in
/// aterm and the bridge reads it on the roster round, so "added" and "in
/// effect" are two moments.
fn subscribe(w: &World, sid: &str, spec: &str) {
    let reply = w.verb(&format!("@{sid} topic add {spec}"));
    assert!(reply.ok(), "topic add {spec}: {}", reply.header());
    let topic = spec.split_whitespace().next().expect("a topic word");
    until("the bridge to sample the topic", || {
        std::fs::read_to_string(w.state.join(format!("topics/{sid}")))
            .ok()
            .filter(|s| s.lines().any(|l| l.split(' ').next() == Some(topic)))
            .map(|_| ())
    });
}

/// The rows of a session's ring that arrived on `topic`.
fn topic_rows(w: &World, sid: &str, topic: &str) -> Vec<String> {
    w.inbox(sid)
        .into_iter()
        .filter(|r| r.contains(&format!(" topic={topic} ")))
        .collect()
}

/// **ONE RECORD, N DELIVERIES — and none at all to a session that never asked.**
///
/// This is the whole contract in one test. A per-recipient fan-OUT would have
/// been far easier to write (the addressed lane already exists, and a broadcast
/// could have been N posts) and it is exactly what §6 forbids: the cost of a
/// shout would then scale with the size of its audience, on an append-forever
/// log, at the sender's choosing. So the assertion is not "everyone got it" —
/// it is "everyone got it AND the log grew by exactly one".
///
/// THE SENDER IS NOT A RECIPIENT. `s_send` publishes the record and is not
/// subscribed, so it receives nothing: delivery is keyed on the receiver's own
/// `topic add` set and on nothing else, which is what makes a fleet-wide shout
/// unable to put a word in front of an agent that did not want it. The same
/// session subscribing later DOES hear its own next broadcast — the rule is
/// "nobody by default", not "anybody but the sender".
#[test]
fn one_record_reaches_every_subscriber_and_nobody_else() {
    let w = World::boot("r23fan", &[]);
    w.wait_ready();
    let (s_a, s_b) = w.two_sessions();
    let spawned = w.verb("spawn");
    assert!(spawned.ok(), "spawn: {}", spawned.header());
    let s_send = spawned
        .header()
        .split_whitespace()
        .nth(1)
        .expect("spawn answers OK <sid>")
        .to_string();
    until("the bridge to see all three sessions", || {
        (w.sessions().len() >= 3).then_some(())
    });

    subscribe(&w, &s_a, "r23.fan");
    subscribe(&w, &s_b, "r23.fan");

    let before = say_records(&w).len();
    let posted = w.verb(&format!("@{s_send} post to=say:r23.fan kind=note r23shout"));
    assert!(posted.ok(), "{}", posted.header());

    for sid in [&s_a, &s_b] {
        let rows = until("the broadcast to reach a subscriber", || {
            let rows = topic_rows(&w, sid, "r23.fan");
            (!rows.is_empty()).then_some(rows)
        });
        assert_eq!(rows.len(), 1, "exactly one row per subscriber: {rows:?}");
        assert!(rows[0].contains("kind=note"), "{}", rows[0]);
    }
    // THE LOG GREW BY EXACTLY ONE. Measured after both deliveries landed, so a
    // fan-OUT would already have written its second record.
    let after = say_records(&w);
    assert_eq!(
        after.len(),
        before + 1,
        "one broadcast is one record, whatever the audience: {after:?}"
    );

    // THE SENDER ASKED FOR NOTHING AND GOT NOTHING.
    assert!(
        topic_rows(&w, &s_send, "r23.fan").is_empty(),
        "the publisher is not a subscriber by virtue of publishing: {:?}",
        w.inbox(&s_send)
    );

    // …AND HEARS ITS OWN NEXT ONE ONCE IT ASKS.
    subscribe(&w, &s_send, "r23.fan");
    let posted = w.verb(&format!("@{s_send} post to=say:r23.fan kind=note r23own"));
    assert!(posted.ok(), "{}", posted.header());
    let mine = until("the sender's own broadcast to come back", || {
        let rows = topic_rows(&w, &s_send, "r23.fan");
        (!rows.is_empty()).then_some(rows)
    });
    assert_eq!(mine.len(), 1, "{mine:?}");
}

/// **A LATE SUBSCRIBER READS THE BACKLOG; `since=head` DOES NOT.**
///
/// `head` is the default because the surprising answer is the dangerous one: an
/// agent that says `topic add build.failed` and is handed six months of build
/// failures has had its context filled by a stranger's history. `@<off>` is the
/// deliberate opposite — a session joining a conversation that has already
/// started — and the offset is the broker's own, the same correlation id every
/// `msg` row already carries, so "from where I stopped reading" is expressible.
#[test]
fn a_late_subscriber_replays_and_head_takes_only_new() {
    let w = World::boot("r23late", &[]);
    w.wait_ready();
    let (s_a, s_b) = w.two_sessions();

    // Published BEFORE anybody asked for the topic: no session on this node
    // holds it, so nothing is delivered and nothing is owed.
    let posted = w.verb(&format!("@{s_a} post to=say:r23.late kind=note r23backlog"));
    assert!(posted.ok(), "{}", posted.header());
    let (off, _, body) = until("the backlog record to land", || {
        say_records(&w)
            .into_iter()
            .find(|(_, _, b)| b.contains("r23backlog"))
    });
    assert!(body.contains("kind=note"), "{body}");

    subscribe(&w, &s_a, &format!("r23.late since=@{off}"));
    subscribe(&w, &s_b, "r23.late since=head");

    // A NEW RECORD WHILE THE BACKLOG IS STILL OWED. This is not decoration: a
    // cursor that is BEHIND the live face must not be advanced past a record
    // the subscription hands over, or the live one silently cancels the
    // backlog the late subscriber asked for. `s_a` must end with BOTH.
    let posted = w.verb(&format!("@{s_a} post to=say:r23.late kind=note r23fresh"));
    assert!(posted.ok(), "{}", posted.header());

    let both = until("the late subscriber's backlog AND the new record", || {
        let rows = topic_rows(&w, &s_a, "r23.late");
        (rows.len() >= 2).then_some(rows)
    });
    assert_eq!(both.len(), 2, "{both:?}");
    assert!(
        both.iter().any(|r| r.contains(&format!("off={off} ")))
            && both.iter().any(|r| r.contains("text=r23fresh")),
        "the replay and the live record both landed: {both:?}"
    );

    // AND THE `head` SUBSCRIBER TOOK ONLY THE NEW ONE — asserted after its own
    // delivery has landed, so the negative is not merely "not yet".
    let fresh = until("the new broadcast to reach the head subscriber", || {
        let rows = topic_rows(&w, &s_b, "r23.late");
        (!rows.is_empty()).then_some(rows)
    });
    assert_eq!(
        fresh.len(),
        1,
        "`since=head` takes the new record and only that: {fresh:?}"
    );
    assert!(fresh[0].contains("text=r23fresh"), "{}", fresh[0]);
}

/// **A BROADCAST IS DATA, WHATEVER IT SAYS IT IS.**
///
/// The body below is a line that would be a HALT if anything read it as one —
/// and a broadcast is the widest reach in the fabric, so it is the worst place
/// for a body to be mistaken for a verb. It arrives as text in a ring, the
/// session's hold is still off, and the screen never saw a keystroke.
#[test]
fn a_broadcast_body_is_data_not_a_verb() {
    let w = World::boot("r23data", &[]);
    w.wait_ready();
    let (s_a, s_b) = w.two_sessions();
    subscribe(&w, &s_b, "r23.data");

    let payload = "hold=1 ◂ @s-1234";
    let posted = w.verb(&format!(
        "@{s_a} post to=say:r23.data kind=note r23danger {payload}"
    ));
    assert!(posted.ok(), "{}", posted.header());

    let rows = until("the record to arrive", || {
        let rows = topic_rows(&w, &s_b, "r23.data");
        (!rows.is_empty()).then_some(rows)
    });
    assert!(
        rows[0].contains("r23danger"),
        "the words arrive as a body: {}",
        rows[0]
    );
    let header = w.verb(&format!("@{s_b} inbox --peek"));
    assert!(
        header.header().contains("hold=0"),
        "a body is not a hold: {}",
        header.header()
    );
    let screen = w.verb(&format!("@{s_b} text"));
    assert!(
        !String::from_utf8_lossy(screen.body()).contains("r23danger"),
        "a broadcast never reaches a keyboard"
    );
}

/// **A `task` FROM A PRINCIPAL THE NODE DOES NOT ACCEPT IS A `note`, on the
/// broadcast lane exactly as on the addressed one.**
///
/// §8.4's demotion is the rule that keeps "a stranger can reach me" from
/// meaning "a stranger can task me", and the kind now rides INSIDE the body of
/// a broadcast rather than in its subject — a body field is the sender's word,
/// which is precisely the class of claim that rule exists to bound. The
/// classification is the same function the addressed path uses
/// (`classify_delivery`), so this is a test that the broadcast path did not
/// grow a second, weaker one.
#[test]
fn a_broadcast_task_is_demoted_unless_the_sender_is_accepted() {
    let w = World::boot("r23kind", &[]);
    w.wait_ready();
    let (_s_a, s_b) = w.two_sessions();
    subscribe(&w, &s_b, "r23.kind");

    let mut god = w.god();
    god.publish(
        7_200,
        1,
        &format!("/f/{FLEET}/pub/n-rogue/s-stranger/say/r23.kind"),
        b"v=1 t=1 kind=task from=s-stranger text=r23stranger",
    )
    .expect("publish as a stranger node");
    god.publish(
        7_201,
        2,
        &format!("/f/{FLEET}/pub/h-andrew/s-person/say/r23.kind"),
        b"v=1 t=1 kind=task from=s-person text=r23human",
    )
    .expect("publish as a human");
    // The OTHER member of the set, so the test is about the set and not about
    // one word in it (`DEMOTE_UNLESS_ACCEPTED = ["task", "control"]`; `ask` and
    // `answer` are deliberately not in it, and this asserts neither).
    god.publish(
        7_202,
        3,
        &format!("/f/{FLEET}/pub/n-rogue/s-stranger/say/r23.kind"),
        b"v=1 t=1 kind=control from=s-stranger text=r23ctl",
    )
    .expect("publish a control as a stranger node");

    let rows = until("all three broadcasts to arrive", || {
        let rows = topic_rows(&w, &s_b, "r23.kind");
        (rows.len() >= 3).then_some(rows)
    });
    let stranger = rows
        .iter()
        .find(|r| r.contains("r23stranger"))
        .expect("the stranger's row");
    assert!(
        stranger.contains("kind=note") && stranger.contains("demoted=task"),
        "an unaccepted principal's broadcast task is a note: {stranger}"
    );
    let human = rows
        .iter()
        .find(|r| r.contains("r23human"))
        .expect("the human's row");
    assert!(
        human.contains("kind=task") && !human.contains("demoted="),
        "a human's task survives on the broadcast lane too: {human}"
    );
    let ctl = rows
        .iter()
        .find(|r| r.contains("r23ctl"))
        .expect("the stranger's control row");
    assert!(
        ctl.contains("kind=note") && ctl.contains("demoted=control"),
        "an unaccepted principal's broadcast control is a note: {ctl}"
    );
}

/// **THE OPT-IN AND ITS CURSOR SURVIVE THE BRIDGE.**
///
/// A topic set that lived only in the bridge's memory would be re-read from
/// `topic ls` after a restart and its `since=head` resolved AGAIN — against the
/// new head — so every broadcast published while the bridge was down would be
/// silently swallowed, and the session that asked for the topic would have no
/// way to know it had missed one. The set is aterm's; the CURSOR is the
/// bridge's, and the cursor is what is persisted.
///
/// THE ASSERTION IS THE CURSOR ITSELF, not a delivery, because the delivery
/// alone cannot tell the two behaviours apart without racing the supervisor:
/// the replacement bridge is brought up in milliseconds, and a record published
/// into that window may arrive live. The cursor is exact. It is read after the
/// replacement has attached — which is the moment a re-resolving bridge would
/// have overwritten it — and the live head is read beside it, so
/// "not fast-forwarded" is a real distinction and not an accident of an idle
/// bus.
#[test]
fn a_restarted_bridge_resumes_the_topic_where_it_stopped() {
    let w = World::boot("r23boot", &[]);
    w.wait_ready();
    let (s_a, s_b) = w.two_sessions();
    subscribe(&w, &s_b, "r23.boot");

    // One delivery first, so the cursor is a POSITION rather than the floor it
    // started at.
    let posted = w.verb(&format!("@{s_a} post to=say:r23.boot kind=note r23first"));
    assert!(posted.ok(), "{}", posted.header());
    let first = until("the first broadcast", || {
        topic_rows(&w, &s_b, "r23.boot")
            .first()
            .and_then(|r| r.split_whitespace().find_map(|t| t.strip_prefix("off=")))
            .and_then(|off| off.parse::<u64>().ok())
    });
    // `<topic> <next> <serial>`; the serial is the endpoint's and not asserted.
    let cursor = |w: &World| {
        std::fs::read_to_string(w.state.join(format!("topics/{s_b}")))
            .unwrap_or_default()
            .trim()
            .rsplit_once(' ')
            .map(|(head, _serial)| head.to_string())
            .unwrap_or_default()
    };
    // THE ROW AND THE CURSOR ARE TWO MOMENTS: the endpoint holds the row as
    // soon as `deliver` answers, and the bridge persists the cursor just after.
    // The assertion is still exact — one past the record — it just waits for it
    // rather than racing the write.
    until("the cursor to move past the delivered record", || {
        (cursor(&w) == format!("r23.boot {}", first + 1)).then_some(())
    });

    let pid = w.bridge_pid();
    harness::kill(pid, 9);
    // INTO THE WINDOW: published while the supervisor is bringing a replacement
    // up. Whether it is read live or replayed, it must arrive.
    let mut god = w.god();
    god.publish(
        7_300,
        1,
        &format!("/f/{FLEET}/pub/n-elsewhere/s-peer/say/r23.boot"),
        b"v=1 t=1 kind=note from=s-peer text=r23gap",
    )
    .expect("publish into the window");
    until("the bridge to come back under a new pid", || {
        let back: i32 = std::fs::read_to_string(w.state.join("pid"))
            .ok()?
            .trim()
            .parse()
            .ok()?;
        (back != pid).then_some(())
    });
    until("the replacement bridge to attach", || {
        w.verb("status")
            .header()
            .contains("fabric=connected")
            .then_some(())
    });

    // THE CURSOR WAS NOT RE-RESOLVED. The head has moved well past it — the
    // attach alone republishes presence — so a bridge that re-read `since=head`
    // would be holding a much larger number here.
    let head = {
        let (_, (_, head)) = god
            .fetch(0, &format!("/f/{FLEET}/>"), 0)
            .expect("the head query");
        head
    };
    assert!(
        head > first + 1,
        "the bus moved past the cursor while the bridge was away ({head} vs {})",
        first + 1
    );
    let after = cursor(&w);
    assert!(
        after.starts_with("r23.boot ") && after != format!("r23.boot {head}"),
        "the restart resumed the topic instead of skipping to head: {after}"
    );

    let rows = until("the record published in the window", || {
        let rows = topic_rows(&w, &s_b, "r23.boot");
        (rows.len() >= 2).then_some(rows)
    });
    assert!(
        rows.iter().any(|r| r.contains("r23gap")),
        "nothing published across a restart is lost: {rows:?}"
    );
}

/// The `msg` rows of a session's ring that arrived on `topic`, with their
/// broker offsets.
fn topic_offsets(w: &World, sid: &str, topic: &str) -> Vec<u64> {
    topic_rows(w, sid, topic)
        .iter()
        .filter_map(|r| r.split_whitespace().find_map(|t| t.strip_prefix("off=")))
        .filter_map(|o| o.parse().ok())
        .collect()
}

/// **A LIVE RECORD DOES NOT CANCEL A BACKLOG STILL OWED.**
///
/// A `since=@<off>` deeper than one roster round can walk (four pages of 256)
/// leaves the cursor BEHIND the live face between rounds. A live shout in that
/// window used to be delivered to the behind cursor and move it to `live + 1`,
/// past everything the walk had not reached — 76 of 1100 records lost, nothing
/// recorded anywhere. The rule now: a behind cursor is not a live recipient;
/// the catch-up owns it, and delivers the live record too, in offset order.
///
/// Sixty-four per sender, to stay under the ring's per-sender quota — the
/// property under test is the cursor, not the ring.
#[test]
fn a_live_record_does_not_cancel_a_backlog_still_owed() {
    let w = World::boot("r23deep", &[]);
    w.wait_ready();
    let (s_a, s_b) = w.two_sessions();
    let mut god = w.god();
    let mut first = None;
    for i in 0..1100u64 {
        let node = format!("n-b{:02}", i / 64);
        let (off, _deduped) = god
            .publish(
                8_000 + i / 64,
                (i % 64) + 1,
                &format!("/f/{FLEET}/pub/{node}/s-bl/say/r23.deep"),
                format!("v=1 t=1 kind=note from=s-bl text=r23bl-{i}").as_bytes(),
            )
            .expect("publish the backlog");
        first.get_or_insert(off);
    }
    let first = first.expect("a first offset");

    subscribe(&w, &s_b, &format!("r23.deep since=@{first}"));
    // THE LIVE SHOUT, while the walk still owes most of the backlog.
    let posted = w.verb(&format!("@{s_a} post to=say:r23.deep kind=note r23live"));
    assert!(posted.ok(), "{}", posted.header());

    let rows = until("the whole backlog and the live record", || {
        let rows = topic_rows(&w, &s_b, "r23.deep");
        (rows.iter().any(|r| r.contains("text=r23bl-1099"))
            && rows.iter().any(|r| r.contains("text=r23live")))
        .then_some(rows)
    });
    // NOTHING SKIPPED: the ring holds the newest 512, and their offsets are
    // contiguous on this topic — a jump would show as a hole.
    let mut offs = topic_offsets(&w, &s_b, "r23.deep");
    offs.sort_unstable();
    for pair in offs.windows(2) {
        assert!(
            pair[1] - pair[0] <= 1 + 3,
            "a gap in what arrived ({} → {}): {rows:?}",
            pair[0],
            pair[1]
        );
    }
    let live_off = offs.last().copied().expect("rows");
    assert!(
        rows.iter()
            .any(|r| r.contains(&format!("off={live_off} ")) && r.contains("r23live")),
        "the live record is the newest, delivered in offset order: {rows:?}"
    );
}

/// **A DELIVERY THE ATERM LANE NEVER ANSWERED IS NOT WRITTEN DOWN.**
///
/// `r1_bridge_wound`'s twin for the broadcast lane: with the verb lane cut
/// before the first `deliver`, the cursor stays where it was — the frontier
/// sweep used to move and fsync it anyway — and the replacement bridge
/// delivers the record.
#[test]
fn a_broadcast_the_lane_never_took_is_redelivered_by_the_replacement() {
    let w = World::boot_with(
        "r23lose",
        &[],
        &[("ATERM_LINK_FAULT", "lose-aterm-before-deliver")],
    );
    w.wait_ready();
    let (_s_a, s_b) = w.two_sessions();
    subscribe(&w, &s_b, "r23.lose");
    let first = w.bridge_pid();

    w.god()
        .publish(
            8_300,
            1,
            &format!("/f/{FLEET}/pub/n-far/s-peer/say/r23.lose"),
            b"v=1 t=1 kind=note from=s-peer text=r23lost",
        )
        .expect("publish the broadcast");
    until("the bridge to lose its verb lane", || {
        std::fs::read_to_string(w.state.join("fault-fired"))
            .ok()
            .map(|_| ())
    });
    until("the bridge child to exit", || {
        (!harness::alive(first)).then_some(())
    });
    let row = until("the replacement to deliver the record", || {
        topic_rows(&w, &s_b, "r23.lose")
            .into_iter()
            .find(|r| r.contains("text=r23lost"))
    });
    assert!(row.contains("from=s-peer@n-far"), "{row}");
}

/// **`drop` THEN `add since=@<off>` INSIDE ONE ROSTER ROUND TAKES EFFECT.**
///
/// The documented way to ask for a different starting point. It was a silent
/// no-op when both happened between two samples: the name survived, the old
/// cursor was kept, the new `since=` never read. The add's serial (`serial=`)
/// makes the re-add a new entry to the bridge.
#[test]
fn drop_then_add_in_one_round_replays_from_the_new_offset() {
    let w = World::boot("r23readd", &[]);
    w.wait_ready();
    let (s_a, s_b) = w.two_sessions();
    let posted = w.verb(&format!("@{s_a} post to=say:r23.readd kind=note r23old"));
    assert!(posted.ok(), "{}", posted.header());
    let (off, _, _) = until("the old record to land", || {
        say_records(&w)
            .into_iter()
            .find(|(_, _, b)| b.contains("r23old"))
    });
    subscribe(&w, &s_b, "r23.readd since=head");

    // BACK TO BACK, no roster round between.
    let dropped = w.verb(&format!("@{s_b} topic drop r23.readd"));
    assert!(
        dropped.header().contains("dropped=1"),
        "{}",
        dropped.header()
    );
    let added = w.verb(&format!("@{s_b} topic add r23.readd since=@{off}"));
    assert!(added.header().contains("added=1"), "{}", added.header());

    let rows = until("the old record, replayed under the new entry", || {
        let rows = topic_rows(&w, &s_b, "r23.readd");
        rows.iter()
            .any(|r| r.contains("text=r23old"))
            .then_some(rows)
    });
    assert_eq!(rows.len(), 1, "{rows:?}");
}

/// **A BROADCAST THE QUOTA REFUSES COUNTS IN THE RECIPIENT'S `dropped=`.**
///
/// One sender, a hundred shouts: 64 rows land and the 65th is `ERR quota`.
/// Nothing goes back to the bus — the sender did not choose this recipient —
/// so the recipient is the only party who can be told, and it is.
#[test]
fn a_quota_refused_broadcast_is_counted_as_dropped() {
    let w = World::boot("r23quota", &[]);
    w.wait_ready();
    let (_s_a, s_b) = w.two_sessions();
    subscribe(&w, &s_b, "r23.quota");
    let mut god = w.god();
    for i in 0..100u64 {
        god.publish(
            8_400,
            i + 1,
            &format!("/f/{FLEET}/pub/n-loud/s-one/say/r23.quota"),
            format!("v=1 t=1 kind=note from=s-one text=r23q-{i}").as_bytes(),
        )
        .expect("publish");
    }
    let header = until("64 rows and 36 refusals", || {
        let h = w.verb(&format!("@{s_b} inbox --peek --meta"));
        let header = h.header().to_string();
        (topic_rows(&w, &s_b, "r23.quota").len() == 64 && header.contains("dropped=36"))
            .then_some(header)
    });
    assert!(header.contains("dropped=36"), "{header}");
    // AND THE BUS HOLDS EXACTLY THE HUNDRED: no refusal record per subscriber.
    let on_bus = say_records(&w)
        .iter()
        .filter(|(_, s, _)| s.ends_with("/say/r23.quota"))
        .count();
    assert_eq!(on_bus, 100);
    let evs = w.ev();
    assert!(
        !evs.iter().any(|e| e.contains("quota")),
        "a broadcast refusal is not an `ev`: {evs:?}"
    );
}
