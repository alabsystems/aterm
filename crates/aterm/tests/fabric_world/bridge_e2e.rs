// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! End-to-end tests for cross-aterm `aterm link serve` messaging over an `astream` broker.

#![cfg(unix)]

use crate::harness;

use harness::{FLEET, World, kv, until};

/// The first cross-aterm message: `post to=@s-B kind=ask --wait` answers `OK <id>
/// off=<R>`, the record lands in B's inbox as `from=<A sid>@<node> trust=agent
/// off=<R>`, B answers `re=R`, and A's drain shows it with `re-id=<the post id>`.
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

    let row = until("the ask to land in B's inbox", || {
        w.inbox(&b).into_iter().find(|r| r.contains("kind=ask"))
    });
    assert!(row.contains(&format!("off={ask_off}")), "{row}");
    assert!(row.contains(&format!("from={a}@{}", w.node)), "{row}");
    assert!(row.contains("trust=agent"), "{row}");
    assert!(row.contains("text=which%20branch"), "{row}");

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

/// A `task` from an unlisted principal arrives `demoted=task`: delivered, but never as
/// an instruction.
#[test]
fn a_task_from_an_unlisted_principal_arrives_demoted() {
    let w = World::boot("demote", &["a-orchestrator"]);
    w.wait_ready();
    let (a, _b) = w.two_sessions();

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

/// Three shapes that never reach an inbox, each recorded instead:
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

    let wanted = [
        format!("undeliverable off={} reason=malformed", offs[0]),
        format!("undeliverable off={} reason=not-hosted", offs[1]),
        format!("undeliverable off={} reason=forged-self", offs[2]),
        format!("cap-compromised off={}", offs[2]),
    ];
    until("every refusal to be recorded", || {
        let ev = w.ev();
        wanted
            .iter()
            .all(|want| ev.iter().any(|e| e.starts_with(want.as_str())))
            .then_some(())
    });
    assert_eq!(
        w.inbox(&a).len(),
        before + 1,
        "still exactly one of the four records delivered"
    );
}

/// The fleet halt: a human's `halt state=on` holds every session the node hosts (a
/// `send` answers `ERR halted`), and the node acks the halt at its offset with
/// `state=held`, so `Last{…/node/ack}` with `re=<halt-offset>` counts who obeyed.
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
    assert!(w.verb(&format!("@{a} inbox --peek")).ok());

    let ack = format!("/f/{FLEET}/pub/{}/node/ack", w.node);
    let counted = until("the node to ack the halt at its offset", || {
        w.row(&ack)
            .filter(|b| b.contains(&format!("re={halt_off}")))
    });
    assert!(
        counted.contains("state=held"),
        "the node must count as HELD at the halt's own offset: {counted}"
    );

    god.publish(5_000, 2, &subject, b"v=1 t=1 state=off")
        .expect("lift the halt");
    until("the halt to lift", || {
        w.verb(&format!("@{a} status"))
            .header()
            .contains("hold=0")
            .then_some(())
    });
}

/// A dead broker leaves the halt where it was, and posts queue.
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

    drop(god);
    w.stop_broker();

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

    w.start_broker();
    until("the queued post to land after the reconnect", || {
        w.inbox(&b)
            .into_iter()
            .any(|r| r.contains("text=queued%20while"))
            .then_some(())
    });
    assert!(w.verb(&format!("@{a} status")).header().contains("hold=1"));
}

/// No bus record moves a PTY byte: an `answer`, `task`, `control`, `note` and `ack`
/// from every principal class, each carrying the `re=` and `gen=` a drive record
/// would, leave the terminal untouched.
#[test]
fn no_bus_record_ever_reaches_a_pty() {
    let w = World::boot("pty", &["h-andrew", "s-1"]);
    w.wait_ready();
    let (a, _b) = w.two_sessions();
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
    harness::publish_pipelined(&mut w.god(), &records);
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

/// Every connection carries its own will: a broker restart fires the will of the
/// connection it cut and the reconnect registers a new one, a `SIGKILL` of the
/// bridge then fires exactly that one, and the relaunch publishes `live` above it.
#[test]
fn every_connection_carries_its_own_will_across_a_broker_restart_and_a_kill() {
    let mut w = World::boot("will", &[]);
    w.wait_ready();
    let presence = format!("/f/{FLEET}/pub/{}/node/presence", w.node);
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
    let live_after_gone = |w: &World, inc: u64| -> Option<u64> {
        let h = history(w);
        let gone = h.iter().position(|(s, i)| s == "away" && *i == inc)?;
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

    let _ = std::fs::remove_file(w.state.join("inc"));
    let _ = std::fs::remove_file(w.state.join("inc-lease"));
    let _ = std::fs::remove_file(w.state.join("seq"));
    harness::kill(pid, 9);

    let back_inc = until(
        "the kill's will to fire and the relaunch to outrank it",
        || live_after_gone(&w, second_inc),
    );
    assert!(
        back_inc > second_inc,
        "the relaunch must take a HIGHER incarnation ({back_inc} !> {second_inc})"
    );

    let seq: u64 = std::fs::read_to_string(w.state.join("seq"))
        .expect("the restart reserved a sequence")
        .trim()
        .parse()
        .expect("a number");
    let old_top = astream_broker::will_seq(second_inc).expect("an attached incarnation");
    assert!(
        seq > old_top,
        "the new incarnation's sequences must be above the old one's reserved top: {seq}"
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
    let node_face = format!("/f/{FLEET}/pub/{}/node/presence", w.node);
    let inc: u64 = until("the orphan's own live node row", || {
        let row = w.row(&node_face)?;
        let mine = row.contains("state=live") && kv(&row, "pid") == Some(&*orphan.to_string());
        mine.then(|| kv(&row, "inc")?.parse().ok()).flatten()
    });

    let mut gui = w.gui.take().expect("the instance");
    let _ = gui.kill();
    let _ = gui.wait();
    let gui = w.gui.insert(World::relaunch_gui(&w));
    w.token = World::wait_for_owned_token_of(&w.ctl_sock, gui, &w.gui_log);
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

    std::fs::remove_file(w.state.join("park-retire")).expect("release the orphan");
    until("the orphan to exit", || {
        (!harness::alive(orphan)).then_some(())
    });
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
