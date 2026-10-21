// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **The doctor retires a ghost WITH THE BRIDGE UP, through it.**
#![allow(
    clippy::disallowed_types,
    reason = "test code: a stand-in child may inherit descriptors"
)]

use crate::harness;

use harness::{FLEET, World, presence_body, until};

fn state_of(body: &str) -> Option<String> {
    body.split_whitespace()
        .find_map(|t| t.strip_prefix("state="))
        .map(str::to_string)
}

/// `aterm fabric doctor <args>` against THIS world: the same binary the
/// world's bridge runs, the world's fabric command as `$ATERM_FABRIC_COMMAND`
/// (which is where `fabric doctor` reads its fleet, broker, caps and state dir
/// from), and the world's `$XDG_RUNTIME_DIR` so the instance is discovered.
fn doctor(w: &World, args: &[&str]) -> String {
    let cmd = std::fs::read_to_string(w.tmp.join("fabric.cmd")).expect("the fabric command");
    let cmd = cmd.trim();
    let bin = cmd.split_whitespace().next().expect("the bridge binary");
    let mut doctor = std::process::Command::new(bin);
    doctor
        .arg("fabric")
        .arg("doctor")
        .args(args)
        .env("HOME", w.tmp.join("home"))
        .env("XDG_RUNTIME_DIR", w.tmp.join("run"))
        .env("ATERM_FABRIC_COMMAND", cmd)
        .stdin(std::process::Stdio::null());
    let out = harness::output_within(&mut doctor, "the doctor");
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// **ONE `exited` PER GHOST, PUBLISHED BY THE BRIDGE, AND THE CLI WROTE NO
/// SEQUENCE.**
///
/// A CLI reserving a sequence beside a live bridge is a deduped publish that
/// reports success and appends nothing, so the request goes through the instance to the bridge on the
/// push lane it already holds, and the bridge publishes under its own
/// sequence. Asserted two ways: the row is retired exactly once with its own
/// `inc=`/`epoch=`/`gen=` carried, and the bridge's `ev` says
/// `reason=operator`.
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
    assert!(dry.contains(ghost), "{dry}");
    assert_eq!(
        presence_body(&w, ghost)
            .as_deref()
            .and_then(state_of)
            .as_deref(),
        Some("live")
    );

    doctor(&w, &["--retire-ghosts", "--yes"]);

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
    // The bridge's `ev` row is its own publish, not ordered before the row it
    // retired, and a fetch that errs reads as no rows at all: waited for like
    // the row above, never read once (a loaded run reads `[]`).
    until("the bridge's ev to name the operator", || {
        w.ev()
            .iter()
            .any(|e| e.contains("presence-retired") && e.contains("reason=operator"))
            .then_some(())
    });

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
    let mut sc = harness::connect(&sibling.sock, &sibling.token).expect("connect to the sibling");
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

/// **A GHOST NODE'S ROWS ARE RETIRED AS THAT NODE, NEVER SENT TO ANOTHER
/// NODE'S BRIDGE.** What `aterm fabric` says of a `remote` node: run `aterm
/// fabric doctor --retire-ghosts` under a `[fabric] command` naming that
/// node's state dir and cap. This machine's own aterm keeps running meanwhile,
/// its bridge up as ITS node — the usual case. That bridge retires only its
/// own node's rows, so a ghost row handed to it was dropped there ("no
/// presence row on this node"), unrefused, and the doctor waited out its
/// five seconds and exited 2 with the row still live. Now the row goes to a
/// bridge of its own node or, with none up, the command publishes it itself
/// under that node's state-dir lock and cap: retired once, its own facts
/// carried, and the live instance's sessions untouched.
#[test]
fn a_ghost_nodes_rows_are_retired_as_that_node_beside_a_live_bridge() {
    let w = World::boot("r24ghostnode", &[]);
    w.wait_ready();
    let (a, _b) = w.two_sessions();

    // THE GHOST NODE: a state dir and a node cap of its own, under the
    // world's mint secret, as an aterm that quit under another state dir
    // left them — and one live presence row it advertised.
    let ghost_node = "n-00000000000a11ce";
    let ghost_state = w.tmp.join("ghost-link");
    std::fs::create_dir_all(&ghost_state).expect("the ghost's state dir");
    std::fs::write(ghost_state.join("node"), format!("{ghost_node}\n")).expect("its node id");
    let ghost_cap = w.tmp.join("ghost.cap");
    let caps: String = harness::node_grants(ghost_node)
        .iter()
        .map(|g| harness::cap_line(g))
        .collect();
    std::fs::write(&ghost_cap, caps).expect("its node cap");
    let ghost = "s-0000000000000000a1ce";
    let subject = format!("/f/{FLEET}/pub/{ghost_node}/{ghost}/presence");
    w.god()
        .publish(
            7_401,
            1,
            &subject,
            b"v=1 t=1 inc=3 epoch=e3 gen=g3 state=live hold=0 holder=- attention=-",
        )
        .expect("publish the ghost node's row");
    until("the ghost node's row to be readable", || {
        (w.row(&subject).as_deref().and_then(state_of).as_deref() == Some("live")).then_some(())
    });

    // ITS COMMAND: the world's, with the ghost's cap and state dir.
    let world_cmd = std::fs::read_to_string(w.tmp.join("fabric.cmd")).expect("the fabric command");
    let bin = world_cmd
        .split_whitespace()
        .next()
        .expect("the bridge binary")
        .to_string();
    let ghost_cmd = format!(
        "{bin} link serve --fleet {FLEET} --broker {} --cap-file {} --state {}",
        w.broker_sock,
        ghost_cap.display(),
        ghost_state.display()
    );
    let mut doctor = std::process::Command::new(&bin);
    doctor
        .args(["fabric", "doctor", "--retire-ghosts", "--yes"])
        .env("HOME", w.tmp.join("home"))
        .env("XDG_RUNTIME_DIR", w.tmp.join("run"))
        .env("ATERM_FABRIC_COMMAND", &ghost_cmd)
        .stdin(std::process::Stdio::null());
    let out = harness::output_within(&mut doctor, "the doctor");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(text.contains(ghost), "the row is listed: {text}");
    assert_eq!(out.status.code(), Some(0), "{text}");

    let body = until("the ghost node's row to read exited", || {
        let b = w.row(&subject)?;
        b.contains("state=exited").then_some(b)
    });
    assert!(
        body.contains("inc=3") && body.contains("epoch=e3") && body.contains("gen=g3"),
        "the row's own facts are carried: {body}"
    );
    // The live instance's own sessions are not ghosts, and stay live.
    assert_eq!(
        presence_body(&w, &a)
            .as_deref()
            .and_then(state_of)
            .as_deref(),
        Some("live"),
        "{text}"
    );
}
