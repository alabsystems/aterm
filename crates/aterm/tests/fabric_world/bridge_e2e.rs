// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **A3 — the first cross-aterm message, end to end.**
//!
//! A headless `aterm` launches a REAL `aterm link serve` child over two
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
//! One property is about what survives a `SIGKILL` BETWEEN two specific steps:
//! a post's `Publish` and its `outbox sent`. That window is microseconds wide
//! and racing it from outside would be exactly the flake this codebase
//! refuses, so the bridge kills itself at the named point under
//! `$ATERM_LINK_FAULT`. The signal is real and the crash is real; only the
//! timing is chosen, by the process whose progress defines it.

#![cfg(unix)]

use crate::harness;

use harness::{FLEET, World, kv, until};

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
        "@{a} post to=@{b} kind=ask --wait=60000 which branch"
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
        "@{b} post to=@{a} kind=answer re={ask_off} --wait=60000 fixtures on audit-2"
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
    // accepted beside whatever it lists (§6.6 row 1, §8.4's `h-*`), so a human
    // could never have been the
    // subject of this test. See `bridge_oversize.rs` for the human's half.
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
        w.row(&ack)
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
///
/// THE HOLD IS SAMPLED DURING THE OUTAGE, after the bridge has run its whole
/// link-loss path ([`World::stop_broker`] returns once the bridge is redialing a
/// missing socket). The check after the reconnect cannot stand in for it: the
/// attach re-derives every standing halt from the bus, so it would put back a
/// hold the outage had lifted.
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
    w.stop_broker();

    // The hold STAYS, with the bridge already redialing: nothing on the link
    // loss path may lift it.
    assert!(
        w.verb(&format!("@{a} status")).header().contains("hold=1"),
        "a broker outage lifted the halt — log:\n{}",
        w.log_tail()
    );
    let refused = w.verb(&format!("@{a} send hi"));
    assert!(
        refused.header().starts_with("ERR halted"),
        "a halted session must stay halted while the broker is away: {}",
        refused.header()
    );
    // `post` and `inbox` stay answerable — a halted agent must still be able to
    // send for help — and the post QUEUES rather than failing.
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
    // post lands.
    w.start_broker();
    until("the queued post to land after the reconnect", || {
        w.inbox(&b)
            .into_iter()
            .any(|r| r.contains("text=queued%20while"))
            .then_some(())
    });
    // And it is still held: the attach re-derives the standing halt, and
    // nothing about a reconnect lifts it.
    assert!(w.verb(&format!("@{a} status")).header().contains("hold=1"));
}

/// **ZERO PTY BYTES from any inbox record, and a stale `epoch=` refused.**
///
/// The rung's whole claim: an `answer`,
/// a `task`, a `control`, a `note` and an `ack` from every principal class,
/// each carrying a `re=` and a `gen=` — the fields a body could use to look
/// like a drive record — must move not one byte of the terminal.
///
/// IT USED TO HAVE A SECOND HALF, and losing it is the point rather than a
/// gap. That half published a real `term/in` record under a stale epoch and
/// asserted the refusal, which proved the epoch check was what stood between
/// the bus and the PTY "rather than the absence of a code path". The drive
/// face is now absent, so the absence of a code path is exactly what stands
/// there, and a test that published to `term/in` would be asserting about a
/// subject this node no longer subscribes to.
#[test]
fn no_bus_record_ever_reaches_a_pty() {
    let w = World::boot("pty", &["h-andrew", "s-1"]);
    w.wait_ready();
    let (a, _b) = w.two_sessions();
    // The baseline is a SETTLED screen: the shell's first prompt is up. A shell that
    // starts slowly would otherwise print it between the two reads and look like a
    // record moving the terminal (seen on a loaded machine: `before` blank, `after`
    // holding the prompt, 9.9 s in).
    until("the shell's first prompt", || {
        w.verb(&format!("@{a} text"))
            .rows()
            .iter()
            .any(|row| row.trim_end().ends_with(['$', '#']))
            .then_some(())
    });
    let before = w.verb(&format!("@{a} text")).rows().join("\n");

    let body = "v=1 t=1 re=1 gen=1:0000000000000000 \
                epoch=00000000000000000000000000000000 text=echo%20PWNED%0A";
    let mut records: Vec<harness::Record> = Vec::new();
    for src in [
        "h-andrew",
        "s-00000000000000000001",
        "n-deadbeefdeadbeef",
        "a-svc",
    ] {
        for kind in ["answer", "task", "control", "note", "ack"] {
            let subject = format!("/f/{FLEET}/in/{}/{a}/{src}/{kind}", w.node);
            let seq = records.len() as u64 + 1;
            records.push((6_200, seq, subject, body.as_bytes().to_vec()));
        }
    }
    // Pipelined (`harness::publish_pipelined`): the same twenty, in order.
    harness::publish_pipelined(&mut w.god(), &records);
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
}

/// **`SIGKILL` between `Publish` and `outbox sent`: the peer sees the message
/// ONCE, and the relaunch does not accuse it.**
///
/// The reason `outbox` is a PEEK.
/// The bridge dies holding a published record the endpoint still lists as in
/// flight, so on restart it re-reads the same post and publishes it again — and
/// the second publish carries the SAME `(producer_id, producer_seq)`, because
/// the sequence is reserved against the POST and persisted before the first
/// attempt. The broker deduplicates it, appends nothing, and answers the
/// ORIGINAL offset, which is the offset the sender is finally told.
///
/// THE KILL IS AT THE WINDOW'S EARLIEST POINT: the ack is in, and the offset
/// is not yet remembered as this node's own. The post went to a session on
/// this node, so it sits on the node's own lane, and the relaunch's attach
/// refills B's lane before its drain republishes anything — a record there at
/// an offset `acked` does not hold is what the self-lane check calls a
/// forgery. The in-flight entry written before the publish is what lets the
/// attach re-learn the offset first; without it the relaunch published
/// `undeliverable reason=forged-self` and `cap-compromised` over the node's
/// own post, inside the attach, before the row this test waits for.
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
    // And nothing called it a forgery: an accusation would have been
    // published by the attach, before the delivery waited on above.
    let ev = w.ev();
    assert!(
        !ev.iter()
            .any(|e| e.starts_with("cap-compromised") || e.contains("reason=forged-self")),
        "the relaunch accused the node's own post: {ev:#?}"
    );
}

/// **EVERY CONNECTION CARRIES ITS OWN WILL.** A broker restart fires the will
/// of the connection it cut, and the reconnect registers a new one; `SIGKILL`
/// of the bridge then fires exactly that one; and the relaunch publishes `live`
/// above it.
///
/// A BROKER RESTART IS A PRESENCE EPOCH. A broker reopened on its log fires
/// every will that log holds, so the node row reads `gone inc=<n>` until the
/// bridge's in-process reconnect — the presence bring-up every attach runs —
/// takes `inc=<n+1>`, registers a will on the NEW connection and publishes
/// `live`. A reconnect that skipped the bring-up leaves the row `gone` while
/// mail flows; one that skipped only the will leaves a later crash with no
/// `gone` at all, because the old will is fenced by the new incarnation's own
/// records.
///
/// The will's sequence is the reserved TOP of its incarnation's space, so no
/// ordinary publish can collide with it; the relaunch's `inc+1` is above that
/// top, which is what suppresses a half-open connection's will structurally
/// rather than by a reader fold. And it holds with the state dir WIPED, because
/// the incarnation is `max(local, the bus's own last row) + 1` — the bus
/// remembers what the disk forgot.
///
/// READ AS HISTORY, not with `Last`: `Last` keeps one record per subject, and
/// the relaunch's `live` can overwrite a `gone` before a poll of it looks.
#[test]
fn every_connection_carries_its_own_will_across_a_broker_restart_and_a_kill() {
    let mut w = World::boot("will", &[]);
    w.wait_ready();
    let presence = format!("/f/{FLEET}/pub/{}/node/presence", w.node);
    // Every `(state, inc)` the node row has carried, oldest first.
    let history = |w: &World| -> Vec<(String, u64)> {
        let rows = w
            .read(|c| {
                let mut out = Vec::new();
                let mut from = 0u64;
                loop {
                    let (rows, (next, head)) = c.fetch(from, &presence, 256)?;
                    out.extend(rows);
                    if next >= head || next <= from {
                        return Ok(out);
                    }
                    from = next;
                }
            })
            .expect("fetch the node's presence history");
        rows.into_iter()
            .filter_map(|(_, _, b)| {
                let b = String::from_utf8_lossy(&b).into_owned();
                Some((kv(&b, "state")?.to_string(), kv(&b, "inc")?.parse().ok()?))
            })
            .collect()
    };
    // The first `live` above `inc` that comes after a `gone inc=<inc>`.
    let live_after_gone = |w: &World, inc: u64| -> Option<u64> {
        let h = history(w);
        let gone = h.iter().position(|(s, i)| s == "gone" && *i == inc)?;
        h[gone..]
            .iter()
            .find(|(s, i)| s == "live" && *i > inc)
            .map(|(_, i)| *i)
    };
    let first_inc = until("the first live row", || {
        history(&w)
            .into_iter()
            .find(|(s, _)| s == "live")
            .map(|(_, i)| i)
    });
    let pid = w.bridge_pid();

    // THE BROKER RESTARTS. The cut connection's will fires (`gone` at the first
    // incarnation), and the reconnect — the same process, not a relaunch —
    // outranks it with a higher one.
    w.stop_broker();
    w.start_broker();
    let second_inc = until("the reconnect to outrank the fired will", || {
        live_after_gone(&w, first_inc)
    });
    assert_eq!(
        w.bridge_pid(),
        pid,
        "a broker restart is a reconnect, not a relaunch"
    );

    // WIPE THE STATE DIR's incarnation and sequence, keeping the node id: the
    // relaunch must still not reuse a sequence, because the bus's own last row
    // tells it where it had got to.
    let _ = std::fs::remove_file(w.state.join("inc"));
    let _ = std::fs::remove_file(w.state.join("seq"));
    harness::kill(pid, 9);

    // The will the RECONNECT registered fires on that connection's end, at its
    // incarnation; aterm relaunches the child, which reads the bus, takes a
    // higher one and publishes `live` above the old reserved top.
    let back_inc = until(
        "the kill's will to fire and the relaunch to outrank it",
        || live_after_gone(&w, second_inc),
    );
    assert!(
        back_inc > second_inc,
        "the relaunch must take a HIGHER incarnation ({back_inc} !> {second_inc})"
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
        seq > (second_inc << 32) | 0xFFFF_FFFF,
        "the new incarnation's sequences must be above the old one's reserved top: {seq}"
    );
}

/// **`SIGKILL` `aterm` after a `deliver`, before the `seen`.** The relaunched
/// instance shows the row ONCE — which today means: not at all in the new
/// session, because its sid is new, and never twice anywhere.
///
/// HONEST SCOPE, and it is narrower than the rung's wording. §6.2 describes a
/// refill "on an instance relaunch or a `session-created` with a known sid",
/// but aterm mints a FRESH sid at every launch (`SessionId::generate`,
/// `aterm-session/src/id.rs`) and has no way to ask for one, so a relaunched
/// instance's sessions are sids the bridge has never seen and the old
/// session's rows have no session to be refilled INTO. The refill mechanism
/// itself — the persisted watermark, the fetch from `seen_off + 1`, and the
/// idempotency that makes it safe over rows the ring holds — is proved across a
/// BRIDGE restart, which needs no stable sid (`bridge_recovery.rs`,
/// `a_session_that_existed_when_the_bridge_started_is_still_refilled`). This
/// test pins what IS true across an `aterm` restart and what the rung's
/// safety content actually is — the bus keeps the durable copy, the bridge
/// comes back clean against a live broker, and nothing is duplicated or
/// replayed into a session that did not ask for it.
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
    let gui = w.gui.insert(World::relaunch_gui(&w));
    w.token = World::wait_for_token_of(&w.ctl_sock, gui, &w.gui_log);
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

/// Every record body under `filter`, oldest first, read to the head.
fn bodies_under(w: &World, filter: &str) -> Vec<String> {
    w.read(|c| {
        let mut out = Vec::new();
        let mut from = 0u64;
        loop {
            let (rows, (next, head)) = c.fetch(from, filter, 256)?;
            out.extend(
                rows.into_iter()
                    .map(|(_, _, body)| String::from_utf8_lossy(&body).into_owned()),
            );
            if next >= head || next <= from {
                return Ok(out);
            }
            from = next;
        }
    })
    .unwrap_or_else(|e| panic!("read the records under {filter}: {e}"))
}

/// **A BRIDGE THROUGH AN UPDATE'S ORDERING: the predecessor's aterm goes while
/// its bridge is mid-drain, and the successor's starts at once on the same
/// state dir.**
///
/// That is the order the product allows. From the Commit to the `_exit` the
/// predecessor answers no retirement (its Commit fence), so a bridge draining
/// then is parked in a post's `outbox sent` when its lanes close, with the
/// rest of the drain in hand; and the successor arms its bridge once its
/// control socket binds, waiting for no predecessor's bridge. An update cannot
/// run headless, so the instance is killed and relaunched without waiting for
/// its bridge — the orphan sees what an update's sees, both lanes closing —
/// and the bridge holds itself at that step
/// (`ATERM_LINK_FAULT=park-retire-while-marked`, one shot) until the
/// successor's bridge has started.
///
/// Asserted: the successor's bridge STANDS BY while the orphan lives (the
/// state dir's lock), rather than attaching beside it; the orphan, released,
/// publishes nothing more — the second post of its drain never reaches the bus
/// — and exits; the successor carries the node at the next incarnation, and
/// its own mail moves. Every wait is an `until`, a hang detector.
///
/// Negative controls: without the drain's lost-lane check the orphan
/// publishes the second post after its aterm is gone, and the bus read below
/// finds it; without the state dir's lock the successor's bridge attaches
/// while the orphan is parked, and the rendezvous answers `Attached`.
#[test]
fn an_orphaned_bridge_mid_drain_neither_publishes_more_nor_shares_the_node() {
    #[derive(Debug, PartialEq, Eq)]
    enum Successor {
        StoodBy,
        Attached,
    }
    let mut w = World::boot_with(
        "orphan-tail",
        &[],
        &[("ATERM_LINK_FAULT", "park-retire-while-marked")],
    );
    w.wait_ready();
    let (a, b) = w.two_sessions();

    // TWO POSTS IN ONE DRAIN: queued while the broker is away, so the drain
    // the reattach runs peeks both.
    w.stop_broker();
    for text in ["orphanfirst", "orphansecond"] {
        let queued = w.verb(&format!("@{a} post to=@{b} kind=note {text}"));
        assert!(
            queued.ok(),
            "post while the broker is down: {}",
            queued.header()
        );
    }
    std::fs::write(w.state.join("park-retire"), b"").expect("arm the park");
    w.start_broker();
    until("the bridge to park the first post's retirement", || {
        w.state.join("fault-fired").exists().then_some(())
    });
    let orphan = w.bridge_pid();
    let inc: u64 = std::fs::read_to_string(w.state.join("inc"))
        .expect("the orphan's incarnation")
        .trim()
        .parse()
        .expect("a number");

    // THE INSTANCE GOES — its bridge is not waited for — AND ITS SUCCESSOR
    // COMES UP on the same state dir at once. (Should this test fail from here
    // on, `World`'s drop kills the bridge the state dir's `pid` names, which
    // is the orphan for as long as it holds the dir.)
    let mut gui = w.gui.take().expect("the instance");
    let _ = gui.kill();
    let _ = gui.wait();
    let gui = w.gui.insert(World::relaunch_gui(&w));
    w.token = World::wait_for_token_of(&w.ctl_sock, gui, &w.gui_log);
    let seen = until("the successor's bridge to stand by or attach", || {
        let fs = w.verb("fabric status").header().to_string();
        match (kv(&fs, "state"), kv(&fs, "reason")) {
            (Some("stalled"), Some("state-locked")) => Some(Successor::StoodBy),
            (Some("connected"), _) => Some(Successor::Attached),
            _ => None,
        }
    });
    assert_eq!(
        seen,
        Successor::StoodBy,
        "the successor's bridge ran beside its predecessor's on one state dir"
    );
    assert!(
        harness::alive(orphan),
        "the orphan exited before the successor's bridge started: no overlap was run"
    );

    // RELEASED, the orphan finds its lanes gone at the retirement it was
    // parked in, and goes.
    std::fs::remove_file(w.state.join("park-retire")).expect("release the orphan");
    until("the orphan to exit", || {
        (!harness::alive(orphan)).then_some(())
    });
    let node_face = format!("/f/{FLEET}/pub/{}/node/presence", w.node);
    let successor = until("the successor's bridge to carry the node", || {
        let pid: i32 = std::fs::read_to_string(w.state.join("pid"))
            .ok()?
            .trim()
            .parse()
            .ok()?;
        let row = w.row(&node_face)?;
        (pid != orphan
            && row.contains("state=live")
            && kv(&row, "pid") == Some(pid.to_string().as_str()))
        .then_some(row)
    });
    assert_eq!(
        kv(&successor, "inc"),
        Some((inc + 1).to_string().as_str()),
        "the node moved more than one incarnation past the orphan's: {successor}"
    );
    w.wait_ready();

    // AND ITS MAIL MOVES: a session of the successor posts to itself and
    // reads it back.
    let fresh = until("the successor's boot session", || {
        w.sessions().first().map(|(_, sid, _)| sid.clone())
    });
    w.drained(&fresh);
    until("the successor's own post to be delivered", || {
        w.inbox(&fresh)
            .iter()
            .any(|r| r.contains("text=drain-barrier"))
            .then_some(())
    });

    // WHAT THE ORPHAN PUBLISHED: the post it was retiring, once — and nothing
    // after its aterm went. It has exited, so nothing can add to this.
    let lane = bodies_under(&w, &format!("/f/{FLEET}/in/{}/{b}/>", w.node));
    let count = |text: &str| {
        lane.iter()
            .filter(|body| body.contains(&format!("text={text}")))
            .count()
    };
    assert_eq!(count("orphanfirst"), 1, "{lane:#?}");
    assert_eq!(
        count("orphansecond"),
        0,
        "the orphan published a post after its aterm was gone: {lane:#?}"
    );
}
