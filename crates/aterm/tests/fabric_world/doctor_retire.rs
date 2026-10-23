// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `fabric doctor --retire-ghosts` with the bridge up: the retirement goes through it.
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

/// Retiring a ghost with a live bridge publishes one exited record through the bridge.
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
    until("the bridge's ev to name the operator", || {
        w.ev()
            .iter()
            .any(|e| e.contains("presence-retired") && e.contains("reason=operator"))
            .then_some(())
    });

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

/// The bridge refuses to retire a session hosted by a sibling instance on the same node.
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

    let reply = w.verb(&format!("fabric retire {x}"));
    assert_eq!(reply.header(), "OK requested=1");
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

/// A ghost node's rows are retired as that node without affecting the live instance.
#[test]
fn a_ghost_nodes_rows_are_retired_as_that_node_beside_a_live_bridge() {
    let w = World::boot("r24ghostnode", &[]);
    w.wait_ready();
    let (a, _b) = w.two_sessions();

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
    assert_eq!(
        presence_body(&w, &a)
            .as_deref()
            .and_then(state_of)
            .as_deref(),
        Some("live"),
        "{text}"
    );
}
