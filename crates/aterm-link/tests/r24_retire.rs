// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **ROUND 24 — the doctor retires a ghost WITH THE BRIDGE UP, through it.**

mod harness;

use harness::{until, World, FLEET};

/// The ghost's presence body as the bus holds it.
fn presence_body(w: &World, sid: &str) -> Option<String> {
    let subject = format!("/f/{FLEET}/pub/{}/{sid}/presence", w.node);
    let mut c = w.god();
    let (rows, _) = c.last(&subject, "", 8).ok()?;
    rows.iter()
        .find(|(_, s, _)| *s == subject)
        .map(|(_, _, b)| String::from_utf8_lossy(b).into_owned())
}

fn state_of(body: &str) -> Option<String> {
    body.split_whitespace()
        .find_map(|t| t.strip_prefix("state="))
        .map(str::to_string)
}

/// `aterm-link fabric doctor <args>` against THIS world: the same binary the
/// world's bridge runs, the world's fabric command as `$ATERM_FABRIC_COMMAND`
/// (which is where `fabric doctor` reads its fleet, broker, caps and state dir
/// from), and the world's `$XDG_RUNTIME_DIR` so the instance is discovered.
fn doctor(w: &World, args: &[&str]) -> String {
    let cmd = std::fs::read_to_string(w.tmp.join("fabric.cmd")).expect("the fabric command");
    let cmd = cmd.trim();
    let bin = cmd.split_whitespace().next().expect("the bridge binary");
    let out = std::process::Command::new(bin)
        .arg("fabric")
        .arg("doctor")
        .args(args)
        .env("HOME", w.tmp.join("home"))
        .env("XDG_RUNTIME_DIR", w.tmp.join("run"))
        .env("ATERM_FABRIC_COMMAND", cmd)
        .stdin(std::process::Stdio::null())
        .output()
        .expect("run the doctor");
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// **ONE `exited` PER GHOST, PUBLISHED BY THE BRIDGE, AND THE CLI WROTE NO
/// SEQUENCE.**
///
/// Round 23 refused to retire while a bridge ran: a CLI reserving a sequence
/// beside a live bridge is a deduped publish that reports success and appends
/// nothing. The request now goes through the instance to the bridge on the
/// push lane it already holds, and the bridge publishes under its own
/// sequence. Asserted three ways: the row is retired exactly once with its own
/// `inc=`/`epoch=`/`gen=` carried, the bridge's `ev` says `reason=operator`,
/// and the doctor says it went through the bridge.
#[test]
fn the_doctor_retires_a_ghost_through_a_live_bridge() {
    let w = World::boot("r24retire", &[]);
    w.wait_ready();
    let (a, _b) = w.two_sessions();
    let ghost = "s-deadbeefdeadbeefdead";
    let subject = format!("/f/{FLEET}/pub/{}/{ghost}/presence", w.node);
    w.god()
        .publish(
            7_400,
            1,
            &subject,
            b"v=1 t=1 inc=1 epoch=e7 gen=g7 state=live hold=0 holder=- attention=-",
        )
        .expect("publish the ghost row");
    until("the ghost row to be readable", || {
        (presence_body(&w, ghost)
            .as_deref()
            .and_then(state_of)
            .as_deref()
            == Some("live"))
        .then_some(())
    });

    // DRY FIRST: named, nothing published, and the note says a bridge would do it.
    let dry = doctor(&w, &["--retire-ghosts"]);
    assert!(
        dry.contains(ghost) && dry.contains("nothing published (dry)"),
        "{dry}"
    );
    assert!(dry.contains("hosts a running bridge"), "{dry}");
    assert_eq!(
        presence_body(&w, ghost)
            .as_deref()
            .and_then(state_of)
            .as_deref(),
        Some("live")
    );

    let out = doctor(&w, &["--retire-ghosts", "--yes"]);
    assert!(out.contains("asked the bridge"), "{out}");
    assert!(
        out.contains("published `exited` for 1 row(s) through the bridge"),
        "{out}"
    );

    let body = until("the row to read exited", || {
        let b = presence_body(&w, ghost)?;
        b.contains("state=exited").then_some(b)
    });
    assert!(
        body.contains("inc=1") && body.contains("epoch=e7") && body.contains("gen=g7"),
        "the row's own facts are carried: {body}"
    );
    let retirements = {
        let mut c = w.god();
        let (rows, _) = c
            .fetch(0, &subject, 256)
            .expect("fetch the ghost's subject");
        rows.iter()
            .filter(|(_, _, b)| String::from_utf8_lossy(b).contains("state=exited"))
            .count()
    };
    assert_eq!(retirements, 1, "one record, by the bridge");
    let evs = w.ev();
    assert!(
        evs.iter()
            .any(|e| e.contains("presence-retired") && e.contains("reason=operator")),
        "the bridge's ev names the operator: {evs:?}"
    );

    // A HOSTED SID IS REFUSED AT THE DOOR, and nothing reaches the bus.
    let refused = w.verb(&format!("fabric retire {a}"));
    assert_eq!(refused.header(), format!("ERR hosted {a}"));
    assert_eq!(
        presence_body(&w, &a)
            .as_deref()
            .and_then(state_of)
            .as_deref(),
        Some("live")
    );
    // AND NOW THERE IS NOTHING LEFT.
    let again = doctor(&w, &["--retire-ghosts"]);
    assert!(again.contains("no ghost rows"), "{again}");
}

/// **A SIBLING INSTANCE'S LIVE SESSION IS NOT A GHOST, WHATEVER THE SNAPSHOT
/// SAID.** The door `fabric retire` comes through is one instance's store;
/// the ghost list is the node's. Two instances on one node — A with the
/// bridge, B without — and a snapshot taken before B (re)adopted its session
/// would name it a ghost; A's door passes it, and A's bridge must refuse it
/// against the node's hosted set, read at publish time.
#[test]
fn the_bridge_refuses_to_retire_a_session_a_sibling_instance_hosts() {
    let mut w = World::boot("r24sibling", &[]);
    w.wait_ready();
    let sibling = w.sibling_instance();
    let mut sc = aterm_link::ctl::Ctl::connect(&sibling.sock, &sibling.token)
        .expect("connect to the sibling");
    let x = until("the sibling's boot session", || {
        sc.request("sessions")
            .ok()?
            .rows()
            .first()?
            .split(' ')
            .nth(1)
            .map(str::to_string)
    });
    // Its presence row, LIVE on this node, as a stale snapshot would leave it.
    let subject = format!("/f/{FLEET}/pub/{}/{x}/presence", w.node);
    w.god()
        .publish(
            7_400,
            1,
            &subject,
            b"v=1 t=1 inc=1 epoch=e9 gen=g9 state=live hold=0 holder=- attention=-",
        )
        .expect("publish the row");
    until("the row to be readable", || {
        (presence_body(&w, &x)
            .as_deref()
            .and_then(state_of)
            .as_deref()
            == Some("live"))
        .then_some(())
    });

    // A's door does not host X, so the request reaches A's bridge...
    let reply = w.verb(&format!("fabric retire {x}"));
    assert_eq!(reply.header(), "OK requested=1");
    // ...which refuses it: the node hosts X.
    until("the bridge to refuse the retire", || {
        w.ev().into_iter().find(|e| {
            e.contains("presence-retire-refused")
                && e.contains(&format!("sid={x}"))
                && e.contains("reason=hosted")
        })
    });
    assert_eq!(
        presence_body(&w, &x)
            .as_deref()
            .and_then(state_of)
            .as_deref(),
        Some("live"),
        "the sibling's session was not retired"
    );
}
