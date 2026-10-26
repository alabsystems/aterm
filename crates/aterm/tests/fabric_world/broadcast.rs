// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Broadcast (`say:` topics).

use crate::harness;

use aterm_models_session::{broadcast_cursor_checkpoint_model, broadcast_head_subscription_model};
use aterm_spec::interp;
use harness::{FLEET, World, say_records, topic_rows, until};

/// `topic add` and wait until the bridge has persisted the topic: the endpoint holds the set
/// and the bridge learns it from a push, so "added" and "in effect" are two moments.
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

/// The cursor the bridge persisted for `sid`'s `topic`: where its next read starts.
fn persisted_topic_cursor(w: &World, sid: &str, topic: &str) -> Option<u64> {
    let body = std::fs::read_to_string(w.state.join(format!("topics/{sid}"))).ok()?;
    body.lines().find_map(|line| {
        let (name, cursor) = line.split_once(' ')?;
        (name == topic).then(|| cursor.parse().ok()).flatten()
    })
}

/// One `say` record reaches every subscriber and the log grows by exactly one (§6: no
/// per-recipient fan-out).
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
    let after = say_records(&w);
    assert_eq!(
        after.len(),
        before + 1,
        "one broadcast is one record, whatever the audience: {after:?}"
    );

    assert!(
        topic_rows(&w, &s_send, "r23.fan").is_empty(),
        "the publisher is not a subscriber by virtue of publishing: {:?}",
        w.inbox(&s_send)
    );

    subscribe(&w, &s_send, "r23.fan");
    let posted = w.verb(&format!("@{s_send} post to=say:r23.fan kind=note r23own"));
    assert!(posted.ok(), "{}", posted.header());
    let mine = until("the sender's own broadcast to come back", || {
        let rows = topic_rows(&w, &s_send, "r23.fan");
        (!rows.is_empty()).then_some(rows)
    });
    assert_eq!(mine.len(), 1, "{mine:?}");
}

/// A late subscriber with `since=@<off>` (a broker offset, as every `msg` row carries)
/// replays the backlog; `since=head`, the default, takes only new records, so a subscriber
/// is not handed the topic's history unasked.
#[test]
fn a_late_subscriber_replays_and_head_takes_only_new() {
    let w = World::boot("r23late", &[]);
    w.wait_ready();
    let (s_a, s_b) = w.two_sessions();

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

    let resolved = std::fs::read_to_string(w.state.join(format!("topics/{s_b}")))
        .expect("the head subscription has a persisted cursor");
    let head_cursor = resolved
        .lines()
        .find_map(|line| line.strip_prefix("r23.late "))
        .and_then(|n| n.parse::<u64>().ok())
        .expect("the head subscription cursor is a broker offset");
    assert!(
        head_cursor > off,
        "`since=head` resolved after backlog offset {off}, even if the say reader is behind: {resolved}"
    );
    let model = broadcast_head_subscription_model();
    let mut state = model.init_state();
    assert!(model.fire("PublishBacklog", &mut state));
    assert!(model.action_enabled("AddHead", &state));
    assert!(model.fire("AddHead", &mut state));
    let real_cursor = if head_cursor > off { 1 } else { 0 };
    assert_eq!(real_cursor, state["cursor"]);
    let old = interp::with_buggy(&model, 1);
    let mut stale = old.init_state();
    assert!(old.fire("PublishBacklog", &mut stale));
    assert!(old.fire("AddHead", &mut stale));
    assert_ne!(
        real_cursor, stale["cursor"],
        "the old stale-head decision must be caught"
    );

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
    assert!(model.fire("TakeBacklog", &mut state));
    assert!(old.fire("TakeBacklog", &mut stale));
    let real_backlog = i64::from(fresh.iter().any(|r| r.contains("r23backlog")));
    assert_eq!(real_backlog, state["backlog_delivered"]);
    assert_ne!(real_backlog, stale["backlog_delivered"]);
    assert!(!old.check_invariant("HeadSkipsEarlierRecord", &stale));
}

/// A broadcast body that reads like a verb (`hold=1`) arrives as ring text: the session's
/// hold stays off and the screen never sees it.
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

/// A broadcast `task` or `control` from a principal the node does not accept arrives as
/// `kind=note demoted=<k>` (§8.4), through the same `classify_delivery` the addressed lane
/// uses: on this lane the kind rides in the body, the sender's own word.
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

/// A restarted bridge resumes a topic from its persisted cursor instead of re-resolving
/// `since=head`, so nothing published while it was down is lost.
#[test]
fn a_restarted_bridge_resumes_the_topic_where_it_stopped() {
    let w = World::boot("r23boot", &[]);
    w.wait_ready();
    let (s_a, s_b) = w.two_sessions();
    subscribe(&w, &s_b, "r23.boot");

    let posted = w.verb(&format!("@{s_a} post to=say:r23.boot kind=note r23first"));
    assert!(posted.ok(), "{}", posted.header());
    let first = until("the first broadcast", || {
        topic_rows(&w, &s_b, "r23.boot")
            .first()
            .and_then(|r| r.split_whitespace().find_map(|t| t.strip_prefix("off=")))
            .and_then(|off| off.parse::<u64>().ok())
    });
    let cursor = |w: &World| {
        std::fs::read_to_string(w.state.join(format!("topics/{s_b}")))
            .unwrap_or_default()
            .trim()
            .to_string()
    };
    until("the cursor to move past the delivered record", || {
        (cursor(&w) == format!("r23.boot {}", first + 1)).then_some(())
    });

    let pid = w.bridge_pid();
    harness::kill(pid, 9);
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

/// An unrelated live broadcast may leave a quiet topic's durable cursor behind its in-memory
/// frontier.
#[test]
fn an_irrelevant_checkpoint_gap_replays_safely_and_live_delivery_stays_durable() {
    let w = World::boot("r23checkpoint", &[]);
    w.wait_ready();
    let (s_a, s_b) = w.two_sessions();
    subscribe(&w, &s_a, "r23.noisy");
    subscribe(&w, &s_b, "r23.quiet");

    let model = broadcast_cursor_checkpoint_model();
    let mut state = model.init_state();
    let old = interp::with_buggy(&model, 1);
    let mut buggy = old.init_state();

    let mut stale = false;
    for i in 0..2 {
        let posted = w.verb(&format!("@{s_a} post to=say:r23.noisy kind=note noise{i}"));
        assert!(posted.ok(), "{}", posted.header());
        let rows = until("the noisy topic's live delivery", || {
            let rows = topic_rows(&w, &s_a, "r23.noisy");
            (rows.len() == i + 1).then_some(rows)
        });
        let off = rows[i]
            .split_whitespace()
            .find_map(|part| part.strip_prefix("off="))
            .and_then(|part| part.parse::<u64>().ok())
            .expect("noisy delivery's offset");
        subscribe(&w, &s_a, &format!("r23.barrier{i}"));
        assert!(model.fire("Irrelevant", &mut state));
        assert!(old.fire("Irrelevant", &mut buggy));
        let on_disk =
            persisted_topic_cursor(&w, &s_b, "r23.quiet").expect("quiet topic's durable cursor");
        if on_disk == off + 1 {
            assert!(model.fire("Checkpoint", &mut state));
            assert!(old.fire("Checkpoint", &mut buggy));
        }
        stale = on_disk <= off;
        assert_eq!(
            !stale,
            state["durable"] == state["memory"],
            "real file and model must agree on whether this row was checkpointed"
        );
        if stale {
            break;
        }
    }
    assert!(
        stale,
        "at least one of two adjacent sweeps skips this SID's checkpoint"
    );
    assert!(topic_rows(&w, &s_b, "r23.quiet").is_empty());

    let pid = w.bridge_pid();
    harness::kill(pid, 9);
    assert!(model.fire("Crash", &mut state));
    assert!(old.fire("Crash", &mut buggy));
    while state["memory"] < state["head"] {
        assert!(model.fire("ReplayIrrelevant", &mut state));
        assert!(old.fire("ReplayIrrelevant", &mut buggy));
    }
    let mut god = w.god();
    let (gap_off, _) = god
        .publish(
            7_301,
            1,
            &format!("/f/{FLEET}/pub/n-elsewhere/s-peer/say/r23.quiet"),
            b"v=1 t=1 kind=note from=s-peer text=quiet-gap",
        )
        .expect("publish matching record across restart");
    until("the replacement bridge", || {
        let back: i32 = std::fs::read_to_string(w.state.join("pid"))
            .ok()?
            .trim()
            .parse()
            .ok()?;
        (back != pid).then_some(())
    });
    let first = until("the matching record after restart", || {
        let rows = topic_rows(&w, &s_b, "r23.quiet");
        (rows.len() == 1).then_some(rows)
    });
    assert!(first[0].contains("quiet-gap"), "{first:?}");
    until("the delivered cursor's durable commit", || {
        (persisted_topic_cursor(&w, &s_b, "r23.quiet") == Some(gap_off + 1)).then_some(())
    });
    assert!(model.fire("Deliver", &mut state));
    assert!(old.fire("Deliver", &mut buggy));
    assert_eq!(state["durable"], state["head"]);
    assert!(buggy["durable"] < buggy["head"]);
    assert!(old.fire("Crash", &mut buggy));
    assert_eq!(
        buggy["reoffered"], 1,
        "the missing-write mutant is detected"
    );

    let posted = w.verb(&format!(
        "@{s_a} post to=say:r23.quiet kind=note quiet-live"
    ));
    assert!(posted.ok(), "{}", posted.header());
    let rows = until("the next matching live record", || {
        let rows = topic_rows(&w, &s_b, "r23.quiet");
        (rows.len() == 2).then_some(rows)
    });
    assert!(rows[1].contains("quiet-live"), "{rows:?}");
    assert_eq!(topic_offsets(&w, &s_b, "r23.quiet").len(), 2);
    let live_off = topic_offsets(&w, &s_b, "r23.quiet")[1];
    until("the live delivered cursor's durable commit", || {
        (persisted_topic_cursor(&w, &s_b, "r23.quiet") == Some(live_off + 1)).then_some(())
    });
    assert!(model.fire("Deliver", &mut state));
    assert_eq!(state["durable"], state["head"]);
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
#[test]
fn a_live_record_does_not_cancel_a_backlog_still_owed() {
    const PAGE: u64 = 256;
    let w = World::boot("r23deep", &[]);
    w.wait_ready();
    let (s_a, s_b) = w.two_sessions();
    subscribe(&w, &s_a, "r23.mark");
    let backlog: Vec<harness::Record> = (0..=PAGE)
        .map(|i| {
            let node = format!("n-b{:02}", i / 64);
            (
                8_000 + i / 64,
                (i % 64) + 1,
                format!("/f/{FLEET}/pub/{node}/s-bl/say/r23.deep"),
                format!("v=1 t=1 kind=note from=s-bl text=r23bl-{i}").into_bytes(),
            )
        })
        .collect();
    let mut god = w.god();
    let first = harness::publish_pipelined(&mut god, &backlog)[0].0;
    god.publish(
        8_090,
        1,
        &format!("/f/{FLEET}/pub/n-mark/s-mk/say/r23.mark"),
        b"v=1 t=1 kind=note from=s-mk text=r23mark",
    )
    .expect("publish the marker");
    until("the face to read past the backlog (s_a's marker)", || {
        topic_rows(&w, &s_a, "r23.mark")
            .iter()
            .any(|r| r.contains("text=r23mark"))
            .then_some(())
    });

    // At the head, past the backlog and before anything is owed: this opt-in spends no page,
    // and s_a takes none of the backlog. Held from before it, s_a took all 257 records, one
    // delivery each, which a loaded machine stretched past the minute's wait above.
    subscribe(&w, &s_a, "r23.deep");
    subscribe(&w, &s_b, &format!("r23.deep since=@{first}"));
    let posted = w.verb(&format!("@{s_a} post to=say:r23.deep kind=note r23live"));
    assert!(posted.ok(), "{}", posted.header());
    until("the face to read the shout (s_a's own row)", || {
        topic_rows(&w, &s_a, "r23.deep")
            .iter()
            .any(|r| r.contains("text=r23live"))
            .then_some(())
    });
    subscribe(&w, &s_a, "r23.nudge");

    let last = format!("text=r23bl-{PAGE}");
    let rows = until("the whole backlog and the live record", || {
        let rows = topic_rows(&w, &s_b, "r23.deep");
        (rows.iter().any(|r| r.contains(&last)) && rows.iter().any(|r| r.contains("text=r23live")))
            .then_some(rows)
    });
    let mut offs = topic_offsets(&w, &s_b, "r23.deep");
    offs.sort_unstable();
    let on_log: Vec<u64> = say_records(&w)
        .into_iter()
        .filter(|(_, subject, _)| subject.ends_with("/say/r23.deep"))
        .map(|(off, _, _)| off)
        .collect();
    assert_eq!(offs, on_log, "{rows:?}");
    let live_off = offs.last().copied().expect("rows");
    assert!(
        rows.iter()
            .any(|r| r.contains(&format!("off={live_off} ")) && r.contains("r23live")),
        "the live record is the newest, delivered in offset order: {rows:?}"
    );
}

/// The broadcast twin of `bridge_oversize`'s lost-lane test: with the verb lane cut before
/// the first `deliver`, the cursor stays put and the replacement bridge delivers the record.
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

/// `topic drop` then `topic add since=@<off>` back to back, with no roster round between,
/// replays from the new offset: the endpoint pushes each change on the event lane in order,
/// and the bridge reads the second's `since=`.
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

/// One sender, a hundred broadcasts: 64 rows land and the rest are `ERR quota`, counted in
/// the recipient's `dropped=`.
#[test]
fn a_quota_refused_broadcast_is_counted_as_dropped() {
    let w = World::boot("r23quota", &[]);
    w.wait_ready();
    let (_s_a, s_b) = w.two_sessions();
    subscribe(&w, &s_b, "r23.quota");
    let shouts: Vec<harness::Record> = (0..100u64)
        .map(|i| {
            (
                8_400,
                i + 1,
                format!("/f/{FLEET}/pub/n-loud/s-one/say/r23.quota"),
                format!("v=1 t=1 kind=note from=s-one text=r23q-{i}").into_bytes(),
            )
        })
        .collect();
    harness::publish_pipelined(&mut w.god(), &shouts);
    let header = until("64 rows and 36 refusals", || {
        let h = w.verb(&format!("@{s_b} inbox --peek --meta"));
        let header = h.header().to_string();
        (topic_rows(&w, &s_b, "r23.quota").len() == 64 && header.contains("dropped=36"))
            .then_some(header)
    });
    assert!(header.contains("dropped=36"), "{header}");
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

/// **A RESTARTED BRIDGE WALKS ITS SESSION'S UNHANDLED BROADCASTS AGAIN, AND
/// THE RING THAT STILL HOLDS THEM TAKES NONE TWICE AND COUNTS NO REFUSAL
/// TWICE** (audit r3-07).
#[test]
fn a_restarted_bridge_walks_the_unhandled_broadcasts_again_and_the_ring_takes_none_twice() {
    let w = World::boot("r37walk", &[]);
    w.wait_ready();
    let (_s_a, s_b) = w.two_sessions();
    subscribe(&w, &s_b, "r37.walk");
    let shouts: Vec<harness::Record> = (0..100u64)
        .map(|i| {
            (
                8_700,
                i + 1,
                format!("/f/{FLEET}/pub/n-loud/s-one/say/r37.walk"),
                format!("v=1 t=1 kind=note from=s-one text=r37w-{i}").into_bytes(),
            )
        })
        .collect();
    harness::publish_pipelined(&mut w.god(), &shouts);
    until("64 rows and 36 refusals", || {
        let header = w
            .verb(&format!("@{s_b} inbox --peek --meta"))
            .header()
            .to_string();
        (topic_rows(&w, &s_b, "r37.walk").len() == 64 && header.contains("dropped=36"))
            .then_some(())
    });
    let row_of = |text: &str| {
        topic_rows(&w, &s_b, "r37.walk")
            .into_iter()
            .find(|r| r.ends_with(&format!("text={text}")))
    };
    let field = |row: &str, key: &str| -> u64 {
        row.split_whitespace()
            .find_map(|t| t.strip_prefix(&format!("{key}=")))
            .and_then(|n| n.parse().ok())
            .unwrap_or_else(|| panic!("{key}= on {row}"))
    };
    let tenth = row_of("r37w-9").expect("the tenth row");
    let id = tenth.split_whitespace().nth(1).expect("an id").to_string();
    assert!(w.verb(&format!("@{s_b} inbox seen {id}")).ok());
    let eleventh = field(&row_of("r37w-10").expect("the eleventh row"), "off");
    until("the floor to move past the handled rows", || {
        std::fs::read_to_string(w.state.join(format!("unhandled/{s_b}")))
            .ok()
            .filter(|f| f.trim() == format!("r37.walk {eleventh}"))
            .map(|_| ())
    });

    w.restart_bridge();
    until("the replacement to walk from the floor", || {
        std::fs::read_to_string(&w.gui_log)
            .ok()
            .filter(|log| log.contains(&format!("offering them again from @{eleventh}")))
            .map(|_| ())
    });
    w.god()
        .publish(
            8_701,
            1,
            &format!("/f/{FLEET}/pub/n-other/s-two/say/r37.walk"),
            b"v=1 t=1 kind=note from=s-two text=r37w-barrier",
        )
        .expect("publish the barrier");
    until("the barrier, after the walk", || row_of("r37w-barrier"));

    let rows = topic_rows(&w, &s_b, "r37.walk");
    for i in 0..100u64 {
        let n = rows
            .iter()
            .filter(|r| r.ends_with(&format!("text=r37w-{i}")))
            .count();
        assert_eq!(n, usize::from(i < 64), "r37w-{i}: {rows:#?}");
    }
    assert_eq!(rows.len(), 65, "{rows:#?}");
    let header = w
        .verb(&format!("@{s_b} inbox --peek --meta"))
        .header()
        .to_string();
    assert!(header.contains("dropped=36"), "counted once: {header}");
}
