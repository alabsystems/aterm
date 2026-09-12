// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **A3 — the first cross-aterm message, end to end.**
//!
//! A headless `aterm-gui` launches a REAL `aterm-link serve` child over two
//! inherited `socketpair` ends, and that child talks to an IN-PROCESS guarded
//! `astream` broker. Nothing here is a mock: the endpoint is the shipped one,
//! the bridge is the shipped binary, the broker enforces capabilities, and the
//! `SIGKILL`s are real signals.
//!
//! Everything the rung asserts is a property of the whole path, so every test
//! below boots the whole path.
//!
//! ## No sleeps as synchronisation
//!
//! Every wait is `until <observable state>` — a row on the broker's log, a field
//! in a `status` reply, a file in the bridge's state dir — bounded by a generous
//! deadline that is a HANG DETECTOR, not a performance assertion.
//!
//! ## Where a crash is timed from the inside
//!
//! Two properties are about what survives a `SIGKILL` BETWEEN two specific
//! steps — one on the inbound plane, one on the outbound. That window is microseconds wide and racing it from outside would be
//! exactly the flake this codebase refuses, so the bridge kills itself at the
//! named point under `$ATERM_LINK_FAULT`. The signal is real and the crash is
//! real; only the timing is chosen, by the process whose progress defines it.

#![cfg(unix)]

mod harness;

use harness::{until, World, FLEET};

/// **The first cross-aterm message.** `post to=@s-B kind=ask --wait` answers `OK
/// <id> off=<R>`; the record lands in B's inbox as `from=<A sid>@<node>
/// trust=agent off=<R>`; B answers `re=R` and A's own drain shows it with
/// `re-id=<the post id>`.
///
/// Every hop is doing real work: the endpoint queues the post, the bridge drains
/// `outbox` for the body, publishes under the node's BOUND cap (so the broker
/// checks the producer id against the principal), reads it back off its own
/// durable group, and delivers it — and the offset the asker gets is the broker's
/// own, which is what makes it a correlation id nobody can forge.
#[test]
fn a_post_lands_in_a_peers_inbox_and_its_answer_comes_back() {
    let w = World::boot("ask", &[]);
    w.wait_ready();
    let (a, b) = w.two_sessions();

    let reply = w.verb(&format!(
        "@{a} post to=@{b} kind=ask --wait=30000 which branch"
    ));
    assert!(
        reply.ok(),
        "post: {} — log:\n{}",
        reply.header(),
        w.log_tail()
    );
    let mut head = reply.header().split_whitespace();
    let post_id: u64 = head.nth(1).and_then(|n| n.parse().ok()).expect("OK <id>");
    let ask_off: u64 = head
        .next()
        .and_then(|t| t.strip_prefix("off="))
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("`--wait` must answer an offset: {}", reply.header()));

    // B'S INBOX. `from=` is rendered from the delivered SUBJECT plus the
    // node-attested `from=<sid>` — never from anything a sender wrote.
    let row = until("the ask to land in B's inbox", || {
        w.inbox(&b).into_iter().find(|r| r.contains("kind=ask"))
    });
    assert!(row.contains(&format!("off={ask_off}")), "{row}");
    assert!(row.contains(&format!("from={a}@{}", w.node)), "{row}");
    assert!(row.contains("trust=agent"), "{row}");
    assert!(row.contains("text=which%20branch"), "{row}");

    // B ANSWERS, and A resolves the answer back to its own post through the
    // (post id <-> offset) table the endpoint keeps.
    let answer = w.verb(&format!(
        "@{b} post to=@{a} kind=answer re={ask_off} --wait=30000 fixtures on audit-2"
    ));
    assert!(answer.ok(), "answer: {}", answer.header());
    let back = until("A to drain the answer", || {
        w.inbox(&a).into_iter().find(|r| r.contains("kind=answer"))
    });
    assert!(back.contains(&format!("re={ask_off}")), "{back}");
    assert!(
        back.contains(&format!("re-id={post_id}")),
        "the answer must resolve to A's own post id: {back}"
    );
}

/// **A `task` from an unlisted principal arrives `demoted=task`.** The message is
/// delivered — a stranger may put words in front of an agent — but never as an
/// instruction. A principal on `--accept-from` keeps its kind, which is what
/// makes the demotion a policy rather than a blanket refusal.
#[test]
fn a_task_from_an_unlisted_principal_arrives_demoted() {
    let w = World::boot("demote", &["a-orchestrator"]);
    w.wait_ready();
    let (a, _b) = w.two_sessions();

    // THE STRANGER IS AN AGENT, not a human. `--accept-from` is the allowlist
    // for principals a node has no other reason to trust; a HUMAN principal is
    // accepted beside whatever it lists (§6.6 row 1, §8.4's `h-*`), exactly as
    // `hook::accepted` has always had it, so a human could never have been the
    // subject of this test. See `r1_bridge_wound.rs` for the human's half.
    let mut god = w.god();
    for (who, text) in [("a-stranger", "unlisted"), ("a-orchestrator", "listed")] {
        let subject = format!("/f/{FLEET}/in/{}/{a}/{who}/task", w.node);
        let body = format!("v=1 t=1 text={text}");
        god.publish(
            7_000 + u64::from(who.len() as u32),
            1,
            &subject,
            body.as_bytes(),
        )
        .expect("publish the task");
    }

    let rows = until("both tasks to land", || {
        let rows = w.inbox(&a);
        (rows.len() >= 2).then_some(rows)
    });
    let unlisted = rows
        .iter()
        .find(|r| r.contains("from=a-stranger"))
        .expect("the unlisted task landed");
    assert!(
        unlisted.contains("kind=note") && unlisted.contains("demoted=task"),
        "an unlisted principal's task must arrive demoted: {unlisted}"
    );
    assert!(unlisted.contains("trust=agent"), "{unlisted}");
    let listed = rows
        .iter()
        .find(|r| r.contains("from=a-orchestrator"))
        .expect("the listed task landed");
    assert!(
        listed.contains("kind=task") && !listed.contains("demoted="),
        "an accepted principal's task keeps its kind: {listed}"
    );
}

/// **Three shapes that must never reach an inbox**, each recorded instead.
///
/// * an EIGHT-segment subject whose tail reads as a forged `<src>/<kind>` —
///   `reason=malformed`, because the parse is by position from the left;
/// * a `<sid>` this node does not host — `reason=not-hosted`;
/// * a record on the node's OWN lane under the node's own id at an offset the
///   bridge never acked — `reason=forged-self`, plus a `cap-compromised` ev,
///   because nobody but a co-holder of the node cap could have written it.
#[test]
fn a_forged_subject_never_reaches_an_inbox_and_is_recorded() {
    let w = World::boot("forge", &[]);
    w.wait_ready();
    let (a, _b) = w.two_sessions();
    let before = w.inbox(&a).len();

    let mut god = w.god();
    let forged_tail = format!("/f/{FLEET}/in/{}/{a}/s-1/h-andrew/answer", w.node);
    let unhosted = format!(
        "/f/{FLEET}/in/{}/s-00000000000000000000/h-andrew/task",
        w.node
    );
    let self_lane = format!("/f/{FLEET}/in/{}/{a}/{}/task", w.node, w.node);
    let mut offs = Vec::new();
    for (seq, subject) in [&forged_tail, &unhosted, &self_lane]
        .into_iter()
        .enumerate()
    {
        let body = b"v=1 t=1 text=nope";
        let (off, _) = god
            .publish(9_100, seq as u64 + 1, subject, body)
            .expect("publish the forgery");
        offs.push(off);
    }
    // A SENTINEL that MUST be delivered, published last: it is what proves the
    // three above were refused rather than merely slow. Waiting for it is the
    // synchronisation — there is no sleep here.
    let good = format!("/f/{FLEET}/in/{}/{a}/h-andrew/note", w.node);
    god.publish(9_100, 4, &good, b"v=1 t=1 text=sentinel")
        .expect("publish the sentinel");

    until("the sentinel to land", || {
        w.inbox(&a)
            .into_iter()
            .any(|r| r.contains("text=sentinel"))
            .then_some(())
    });
    let rows = w.inbox(&a);
    assert_eq!(
        rows.len(),
        before + 1,
        "exactly one of the four records may be delivered: {rows:#?}"
    );

    let ev = w.ev();
    for (off, reason) in [
        (offs[0], "malformed"),
        (offs[1], "not-hosted"),
        (offs[2], "forged-self"),
    ] {
        assert!(
            ev.iter()
                .any(|e| e.contains(&format!("undeliverable off={off} reason={reason}"))),
            "no `undeliverable off={off} reason={reason}` in {ev:#?}"
        );
    }
    assert!(
        ev.iter().any(|e| e.starts_with("cap-compromised")),
        "a forged record on our own lane is a COMPROMISE, not a delivery problem: {ev:#?}"
    );
}

/// **The fleet halt.** A human's `halt state=on` holds every session the node
/// hosts — `turn` answers `ERR halted` from an Owner connection — and the node
/// answers the halt at its offset with `state=held`, which is what makes
/// `Last{…/node/ack}` with `re=<halt-offset>` a count of who obeyed.
#[test]
fn a_fleet_halt_holds_every_session_and_is_acked_at_its_offset() {
    let w = World::boot("halt", &[]);
    w.wait_ready();
    let (a, _b) = w.two_sessions();

    let mut god = w.god();
    let subject = format!("/f/{FLEET}/fleet/h-x/halt");
    let (halt_off, _) = god
        .publish(5_000, 1, &subject, b"v=1 t=1 state=on reason=stop")
        .expect("publish the halt");

    until("aterm to answer ERR halted", || {
        let reply = w.verb(&format!("@{a} send hi"));
        reply.header().starts_with("ERR halted").then_some(())
    });
    assert!(
        w.verb(&format!("@{a} status")).header().contains("hold=1"),
        "status must report the standing hold"
    );
    // EXEMPT VERBS STAY ANSWERABLE: a halted agent must still be able to ask why.
    assert!(w.verb(&format!("@{a} inbox --peek")).ok());

    // ONE RETAINED `ack` SUBJECT PER NODE, NAMING THE BARRIER IT ANSWERS.
    // A subject per halt offset spends one of the producer's 4096 distinct
    // subjects on every halt record, on and off, forever — and past that bound
    // the node cannot publish its own `live` presence row, so no bridge on it
    // can ever attach again. `re=` carries the same information at a bounded
    // cost: a quorum fold over `Last{/f/<F>/pub/*/node/ack}` counts the members
    // whose `re=` is this barrier.
    let ack = format!("/f/{FLEET}/pub/{}/node/ack", w.node);
    let counted = until("the node to ack the halt at its offset", || {
        let (rows, _) = w.god().last(&ack, "", 8).ok()?;
        rows.into_iter()
            .find(|(_, s, _)| *s == ack)
            .map(|(_, _, b)| String::from_utf8_lossy(&b).into_owned())
            .filter(|b| b.contains(&format!("re={halt_off}")))
    });
    assert!(
        counted.contains("state=held"),
        "the node must count as HELD at the halt's own offset: {counted}"
    );

    // AND IT LIFTS: each human lifts their own.
    god.publish(5_000, 2, &subject, b"v=1 t=1 state=off")
        .expect("lift the halt");
    until("the halt to lift", || {
        w.verb(&format!("@{a} status"))
            .header()
            .contains("hold=0")
            .then_some(())
    });
}

/// **A dead broker leaves the halt exactly where it was, and the posts queue.**
///
/// This is the direction the failure must go. A hold that a broker outage lifted
/// would mean an agent could halt-dodge by cutting a cable; a post dropped
/// because the broker was away would be a message the sender was told `OK` for
/// and nobody ever sees. Neither happens: the hold lives in the endpoint and only
/// a `state=off` RECORD lifts it, and `post` is refused at the door long before
/// the queue could grow without bound.
#[test]
fn a_dead_broker_keeps_the_hold_and_queues_the_posts_until_it_returns() {
    let mut w = World::boot("outage", &[]);
    w.wait_ready();
    let (a, b) = w.two_sessions();

    let mut god = w.god();
    let subject = format!("/f/{FLEET}/fleet/h-x/halt");
    god.publish(5_100, 1, &subject, b"v=1 t=1 state=on reason=stop")
        .expect("publish the halt");
    until("the halt to reach the endpoint", || {
        w.verb(&format!("@{a} status"))
            .header()
            .contains("hold=1")
            .then_some(())
    });

    // THE BROKER GOES AWAY. Its listener closes and every bridge connection with
    // it; the log file stays, so the same records come back when it returns.
    drop(god);
    if let Some(mut h) = w.handle.take() {
        h.shutdown();
    }
    w.broker.take();

    // The hold STAYS. `post` and `inbox` stay answerable — a halted agent must
    // still be able to send for help — and the post QUEUES rather than failing.
    until("the bridge to notice the broker is gone", || {
        w.verb(&format!("@{a} status"))
            .header()
            .contains("hold=1")
            .then_some(())
    });
    let queued = w.verb(&format!(
        "@{a} post to=@{b} kind=note queued while the bus was down"
    ));
    assert!(
        queued.ok(),
        "post while the broker is down: {}",
        queued.header()
    );
    let listing = w.verb(&format!("@{a} inbox --peek"));
    assert!(
        listing
            .rows()
            .iter()
            .any(|r| r.starts_with("post ") && r.contains("off=-")),
        "the post must be listed as still in flight: {:#?}",
        listing.rows()
    );

    // THE BROKER COMES BACK on the same log and the same path, and the queued
    // post lands. The stale socket file is removed first: `serve` binds, it does
    // not hijack.
    let _ = std::fs::remove_file(&w.broker_sock);
    let broker = astream_broker::Broker::open_guarded(&w.broker_log, harness::SECRET.to_vec())
        .expect("reopen the guarded broker");
    let handle = broker.serve(&w.broker_sock).expect("re-serve");
    w.broker = Some(broker);
    w.handle = Some(handle);

    until("the queued post to land after the reconnect", || {
        w.inbox(&b)
            .into_iter()
            .any(|r| r.contains("text=queued%20while"))
            .then_some(())
    });
    // And it is still held: nothing about a reconnect lifts a standing halt.
    assert!(w.verb(&format!("@{a} status")).header().contains("hold=1"));
}

/// **ZERO PTY BYTES from any inbox record, and a stale `epoch=` refused.**
///
/// The rung's sharpest claim. An `answer` and a `task` from every principal
/// class, each carrying a `re=` and a `gen=` — the two fields a body could use to
/// look like a drive record — must move not one byte of the terminal. Then a real
/// `term/in` record with a STALE epoch is refused and recorded, which is the
/// proof that the epoch check is what stands between the bus and the PTY rather
/// than the absence of a code path.
#[test]
fn no_inbox_record_ever_reaches_the_pty_and_a_stale_epoch_is_refused() {
    let w = World::boot("pty", &["h-andrew", "s-1"]);
    w.wait_ready();
    let (a, _b) = w.two_sessions();
    let before = w.verb(&format!("@{a} text")).rows().join("\n");

    let mut god = w.god();
    let mut seq = 0u64;
    for src in [
        "h-andrew",
        "s-00000000000000000001",
        "n-deadbeefdeadbeef",
        "a-svc",
    ] {
        for kind in ["answer", "task", "control", "note", "ack"] {
            seq += 1;
            let subject = format!("/f/{FLEET}/in/{}/{a}/{src}/{kind}", w.node);
            let body = "v=1 t=1 re=1 gen=1:0000000000000000 \
                        epoch=00000000000000000000000000000000 text=echo%20PWNED%0A";
            god.publish(6_200, seq, &subject, body.as_bytes())
                .expect("publish");
        }
    }
    // Synchronise on the LAST one landing, then assert about the screen.
    until("every inbox record to be handled", || {
        (w.inbox(&a).len() >= 20).then_some(())
    });
    let after = w.verb(&format!("@{a} text")).rows().join("\n");
    assert_eq!(
        before, after,
        "an inbox record moved the terminal — no `in` kind may ever reach a PTY"
    );
    assert!(!after.contains("PWNED"), "{after}");

    // NOW THE DRIVE FACE, with a stale epoch. The human claims control first
    // (§6.6: a claim by an `h-*` principal is granted unconditionally), so the
    // refusal that follows is about the EPOCH and not about the holder.
    let epoch = until("A's launch nonce", || {
        w.sessions()
            .into_iter()
            .find(|(_, sid, _)| *sid == a)
            .map(|(_, _, nonce)| nonce)
    });
    let claim = format!("/f/{FLEET}/in/{}/{a}/h-andrew/control", w.node);
    god.publish(
        6_300,
        1,
        &claim,
        format!("v=1 t=1 epoch={epoch} text=claim").as_bytes(),
    )
    .expect("publish the claim");
    until("the claim to be recorded", || {
        let subject = format!("/f/{FLEET}/pub/{}/{a}/control", w.node);
        let (rows, _) = w.god().last(&subject, "", 8).ok()?;
        rows.into_iter()
            .find(|(_, s, _)| *s == subject)
            .filter(|(_, _, b)| String::from_utf8_lossy(b).contains("holder=h-andrew"))
            .map(|_| ())
    });

    let drive = format!("/f/{FLEET}/term/{}/{a}/in/h-andrew", w.node);
    let stale = "v=1 t=1 epoch=ffffffffffffffffffffffffffffffff len=11\necho PWNED";
    god.publish(6_400, 1, &drive, stale.as_bytes())
        .expect("publish the stale drive record");
    until("the stale epoch to be refused and recorded", || {
        w.ev()
            .into_iter()
            .any(|e| e.contains(&format!("refused sid={a} face=term reason=epoch")))
            .then_some(())
    });
    let after = w.verb(&format!("@{a} text")).rows().join("\n");
    assert!(
        !after.contains("PWNED"),
        "a stale epoch still typed: {after}"
    );
}

/// **`SIGKILL` between `deliver` and `Commit`: the row is there ONCE.**
///
/// The bus commits its group cursor only AFTER `deliver` answers `OK`, so a
/// bridge that dies in that window redelivers the record on restart. The
/// endpoint's idempotency on `off=` is what turns that at-least-once cursor into
/// exactly-once — and aterm's own supervisor is what brings the bridge back, so
/// this also proves the relaunch.
#[test]
fn a_kill_between_deliver_and_commit_leaves_exactly_one_row() {
    let w = World::boot_with("crash", &[], &[("ATERM_LINK_FAULT", "kill-after-deliver")]);
    w.wait_ready();
    let (a, _b) = w.two_sessions();
    let first_pid = w.bridge_pid();

    let mut god = w.god();
    let subject = format!("/f/{FLEET}/in/{}/{a}/h-andrew/note", w.node);
    god.publish(8_000, 1, &subject, b"v=1 t=1 text=once")
        .expect("publish");

    // The bridge delivers it, then kills itself before committing. aterm holds
    // every session it governed and relaunches the child; the new bridge resumes
    // the UNCOMMITTED cursor and redelivers.
    until("the bridge to die and be relaunched", || {
        let pid = std::fs::read_to_string(w.state.join("pid")).ok()?;
        let pid: i32 = pid.trim().parse().ok()?;
        (pid != first_pid).then_some(())
    });
    until("the redelivered row to settle", || {
        w.inbox(&a)
            .into_iter()
            .any(|r| r.contains("text=once"))
            .then_some(())
    });
    // The relaunched bridge is under the SAME fault, so it dies again on the
    // redelivery — and that is the point: however many times the cursor replays
    // the record, the endpoint holds exactly one row for that offset.
    let rows: Vec<String> = w
        .inbox(&a)
        .into_iter()
        .filter(|r| r.contains("text=once"))
        .collect();
    assert_eq!(
        rows.len(),
        1,
        "the redelivered offset must not append twice: {rows:#?}"
    );
}

/// **`SIGKILL` between `Publish` and `outbox sent`: the peer sees the message
/// ONCE.**
///
/// The outbound mirror of the previous test, and the reason `outbox` is a PEEK.
/// The bridge dies holding a published record the endpoint still lists as in
/// flight, so on restart it re-reads the same post and publishes it again — and
/// the second publish carries the SAME `(producer_id, producer_seq)`, because
/// the sequence is reserved against the POST and persisted before the first
/// attempt. The broker deduplicates it, appends nothing, and answers the
/// ORIGINAL offset, which is the offset the sender is finally told.
#[test]
fn a_kill_between_publish_and_outbox_sent_leaves_one_record_on_the_bus() {
    let w = World::boot_with(
        "republish",
        &[],
        &[("ATERM_LINK_FAULT", "kill-after-publish")],
    );
    w.wait_ready();
    let (a, b) = w.two_sessions();
    let first_pid = w.bridge_pid();

    // No `--wait`: the point is what the BUS holds, and the wait would park on a
    // bridge that is about to kill itself.
    let posted = w.verb(&format!("@{a} post to=@{b} kind=note only once please"));
    assert!(posted.ok(), "post: {}", posted.header());

    until("the bridge to die mid-publish and be relaunched", || {
        let pid = std::fs::read_to_string(w.state.join("pid")).ok()?;
        let pid: i32 = pid.trim().parse().ok()?;
        (pid != first_pid).then_some(())
    });
    until("the peer to see the message", || {
        w.inbox(&b)
            .into_iter()
            .any(|r| r.contains("text=only%20once%20please"))
            .then_some(())
    });

    // ONE record on B's lane, and one row in B's inbox. The lane is the sharper
    // assertion: the ring would have deduped a second delivery on `off=`, so a
    // duplicate PUBLISH could hide there — on the log it cannot.
    let lane = format!("/f/{FLEET}/in/{}/{b}/{}/note", w.node, w.node);
    let mut c = w.god();
    let (rows, _) = c.fetch(0, &lane, 64).expect("fetch B's lane");
    assert_eq!(
        rows.len(),
        1,
        "the republish must be deduped away: {rows:#?}"
    );
    let seen: Vec<String> = w
        .inbox(&b)
        .into_iter()
        .filter(|r| r.contains("text=only%20once%20please"))
        .collect();
    assert_eq!(seen.len(), 1, "{seen:#?}");
}

/// **`SIGKILL` the bridge: the will fires exactly one `gone`, and the restart
/// publishes `live inc+1`.**
///
/// The will's sequence is the reserved TOP of its incarnation's space, so no
/// ordinary publish can collide with it; the restart's `inc+1` is above that top,
/// which is what suppresses a half-open connection's will structurally rather
/// than by a reader fold. And it holds with the state dir WIPED, because the
/// incarnation is `max(local, the bus's own last row) + 1` — the bus remembers
/// what the disk forgot.
#[test]
fn killing_the_bridge_publishes_gone_and_the_restart_publishes_live_inc_plus_one() {
    let w = World::boot("will", &[]);
    w.wait_ready();
    let presence = format!("/f/{FLEET}/pub/{}/node/presence", w.node);
    let read = |w: &World| -> Option<String> {
        let (rows, _) = w.god().last(&presence, "", 8).ok()?;
        rows.into_iter()
            .find(|(_, s, _)| *s == presence)
            .map(|(_, _, b)| String::from_utf8_lossy(&b).into_owned())
    };
    let first = until("the first live row", || {
        read(&w).filter(|r| r.contains("state=live"))
    });
    let first_inc = inc_of(&first);
    let pid = w.bridge_pid();

    // WIPE THE STATE DIR's incarnation and sequence, keeping the node id: the
    // restart must still not reuse a sequence, because the bus's own last row
    // tells it where it had got to.
    let _ = std::fs::remove_file(w.state.join("inc"));
    let _ = std::fs::remove_file(w.state.join("seq"));
    harness::kill(pid, 9);

    // The will fires on the connection's end: exactly one `gone`, at this
    // incarnation.
    let gone = until("the will to fire", || {
        read(&w).filter(|r| r.contains("state=gone"))
    });
    assert_eq!(
        inc_of(&gone),
        first_inc,
        "the goodbye names its own incarnation: {gone}"
    );

    // aterm relaunches the child, which reads the bus, takes `inc+1`, and
    // publishes `live` — which is above the old incarnation's reserved top, so
    // the old will can never overwrite it.
    let back = until("the restart to publish live inc+1", || {
        read(&w).filter(|r| r.contains("state=live"))
    });
    assert!(
        inc_of(&back) > first_inc,
        "the restart must take a HIGHER incarnation ({} !> {first_inc}): {back}",
        inc_of(&back)
    );

    // AND THE SEQUENCE DID NOT REPEAT. Every publish of the new incarnation is
    // above `(inc << 32)`, so no record of it can collide with — and be deduped
    // away by — a record of the old one.
    let seq: u64 = std::fs::read_to_string(w.state.join("seq"))
        .expect("the restart reserved a sequence")
        .trim()
        .parse()
        .expect("a number");
    assert!(
        seq > (first_inc << 32) | 0xFFFF_FFFF,
        "the new incarnation's sequences must be above the old one's reserved top: {seq}"
    );
}

/// **The endpoint watermark survives the endpoint.** `inbox seen` is persisted by
/// the bridge; on a restart the bridge refills each session's ring from
/// `seen_off + 1`, and `deliver`'s idempotency means doing that unconditionally
/// leaves exactly one row per offset.
///
/// HONEST SCOPE, and it is narrower than the rung's wording. §6.2 describes the
/// refill happening "on an instance relaunch or a `session-created` with a known
/// sid", and the rung asks for a `SIGKILL` of `aterm-gui` itself. aterm mints a
/// FRESH sid at every launch (`SessionId::generate`, `aterm-session/src/id.rs`)
/// and has no way to ask for one, so a relaunched instance's sessions are sids
/// the bridge has never seen and the "known sid" case cannot arise across a real
/// relaunch today. What is proved here is the mechanism the rung is about — the
/// persisted watermark, the refill from it, and the idempotency that makes the
/// refill safe — across a bridge restart, which is the half that does not need a
/// stable sid. The `aterm-gui` half is
/// [`killing_aterm_gui_loses_no_row_and_duplicates_none`].
#[test]
fn the_persisted_watermark_refills_a_ring_without_duplicating_a_row() {
    let w = World::boot("refill", &[]);
    w.wait_ready();
    let (a, _b) = w.two_sessions();

    let mut god = w.god();
    let subject = format!("/f/{FLEET}/in/{}/{a}/h-andrew/note", w.node);
    for seq in 1..=3u64 {
        god.publish(
            8_400,
            seq,
            &subject,
            format!("v=1 t=1 text=row{seq}").as_bytes(),
        )
        .expect("publish");
    }
    until("all three rows to land", || {
        (w.inbox(&a).len() >= 3).then_some(())
    });

    // Mark the FIRST handled. The bridge persists that watermark off the events
    // digest, keyed by sid.
    let first_id = w
        .inbox(&a)
        .first()
        .and_then(|r| r.split_whitespace().nth(1).map(str::to_string))
        .expect("a row id");
    assert!(w.verb(&format!("@{a} inbox seen {first_id} handled")).ok());
    until("the bridge to persist the watermark", || {
        w.state.join("seen").join(&a).exists().then_some(())
    });

    let pid = w.bridge_pid();
    harness::kill(pid, 9);
    until("the bridge to come back", || {
        let back = std::fs::read_to_string(w.state.join("pid")).ok()?;
        let back: i32 = back.trim().parse().ok()?;
        (back != pid).then_some(())
    });
    until("the restarted bridge to be connected", || {
        w.verb("status")
            .header()
            .contains("fabric=connected")
            .then_some(())
    });

    // The refill re-offers rows 2 and 3 (and, harmlessly, anything at or above
    // the watermark); every one of them is an offset the ring already holds, so
    // the count does not move.
    let rows = w.inbox(&a);
    assert_eq!(
        rows.len(),
        3,
        "a refill must not duplicate a row it already delivered: {rows:#?}"
    );
    for n in 1..=3 {
        assert_eq!(
            rows.iter()
                .filter(|r| r.contains(&format!("text=row{n}")))
                .count(),
            1,
            "row{n} appears more than once: {rows:#?}"
        );
    }
}

/// **`SIGKILL` `aterm-gui` after a `deliver`, before the `seen`.** The relaunched
/// instance shows the row ONCE — which today means: not at all in the new
/// session, because its sid is new, and never twice anywhere.
///
/// See [`the_persisted_watermark_refills_a_ring_without_duplicating_a_row`] for
/// why the "shows the row once" half cannot be asserted as the rung words it: a
/// relaunched aterm mints fresh sids, so the old session's rows have no session
/// to be refilled INTO. This test pins what IS true and what the rung's safety
/// content actually is — the bus keeps the durable copy, the bridge comes back
/// clean against a live broker, and nothing is duplicated or replayed into a
/// session that did not ask for it.
#[test]
fn killing_aterm_gui_loses_no_row_and_duplicates_none() {
    let mut w = World::boot("relaunch", &[]);
    w.wait_ready();
    let (a, _b) = w.two_sessions();

    let mut god = w.god();
    let subject = format!("/f/{FLEET}/in/{}/{a}/h-andrew/note", w.node);
    god.publish(8_800, 1, &subject, b"v=1 t=1 text=unseen")
        .expect("publish");
    until("the row to be delivered", || {
        w.inbox(&a)
            .into_iter()
            .any(|r| r.contains("text=unseen"))
            .then_some(())
    });
    // Deliberately NOT `inbox seen`: the row is delivered and unhandled, which is
    // the window the rung names.

    let mut gui = w.gui.take().expect("the instance");
    let _ = gui.kill();
    let _ = gui.wait();
    // The bridge's descriptors close with it, so the bridge exits too.
    until("the bridge to exit with the instance", || {
        let pid = w.bridge_pid();
        (!harness::alive(pid)).then_some(())
    });

    // RELAUNCH against the same broker, the same state dir, the same node id.
    let relaunched = World::relaunch_gui(&w);
    w.gui = Some(relaunched);
    w.token = World::wait_for_token_at(&w.ctl_sock);
    w.wait_ready();

    // The record is still on the bus — the durable copy the design promises —
    // and the group cursor has already passed it, so it is not re-delivered into
    // the fresh session that does not own it.
    let mut c = w.god();
    let (rows, _) = c.fetch(0, &subject, 64).expect("fetch the lane");
    assert_eq!(rows.len(), 1, "the bus keeps exactly one copy: {rows:#?}");
    let fresh: Vec<String> = w
        .sessions()
        .into_iter()
        .flat_map(|(_, sid, _)| w.inbox(&sid))
        .filter(|r| r.contains("text=unseen"))
        .collect();
    assert!(
        fresh.is_empty(),
        "a relaunched instance's NEW sid must not inherit another session's mail: {fresh:#?}"
    );
}

/// The `inc=` on a presence body.
fn inc_of(row: &str) -> u64 {
    row.split_whitespace()
        .find_map(|t| t.strip_prefix("inc="))
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("no inc= in {row}"))
}
