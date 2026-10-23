// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Broker link-state reporting and reconnect tests against a live headless instance.

use crate::harness;

use std::io::Read as _;
use std::os::unix::net::UnixListener;
use std::time::{Duration, Instant};

use astream_broker::Broker;
use harness::{FLEET, SECRET, World, cap_line, kv, node_grants, until};

/// `status`'s header for the boot session.
fn status(w: &World) -> String {
    w.verb("status").header().to_string()
}

/// `fabric status`'s header (Owner-only; the harness token is Owner).
fn fabric_status(w: &World) -> String {
    w.verb("fabric status").header().to_string()
}

/// The boot session's sid.
fn boot_sid(w: &World) -> String {
    until("aterm's boot session", || {
        w.sessions().first().map(|(_, sid, _)| sid.clone())
    })
}

/// on the same log and the same socket path.
#[test]
fn a_killed_broker_stalls_the_fabric_and_a_restart_reconnects() {
    let mut w = World::boot("r13-kill", &[]);
    w.wait_ready();
    let sid = boot_sid(&w);

    let s = status(&w);
    assert!(s.contains(" fabric=connected "), "{s}");
    let rtt: u64 = kv(&s, "fabric_rtt_ms")
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("connected must carry a numeric fabric_rtt_ms=: {s}"));
    let age: u64 = kv(&s, "fabric_link_age_ms")
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("connected must carry a numeric fabric_link_age_ms=: {s}"));
    assert!(rtt >= 1, "a real ack never reads 0 ms: {s}");
    eprintln!("MEASURED publish ack round trip over the Unix socket: {rtt} ms (age {age} ms)");
    let fs = fabric_status(&w);
    assert_eq!(kv(&fs, "state"), Some("connected"), "{fs}");
    assert_eq!(kv(&fs, "reason"), Some("-"), "{fs}");
    assert_eq!(kv(&fs, "rtt_ms"), Some(rtt.to_string().as_str()), "{fs}");
    let lane = aterm_types::control_verbs::BRIDGE_LANE.to_string();
    assert_eq!(kv(&fs, "bridge_lane"), Some(lane.as_str()), "{fs}");
    assert!(kv(&fs, "build").is_some(), "{fs}");
    assert_eq!(kv(&fs, "bridge_build"), kv(&fs, "build"), "{fs}");
    assert_eq!(kv(&fs, "lane_faults"), Some("0"), "{fs}");

    if let Some(mut h) = w.handle.take() {
        h.shutdown();
    }
    w.broker.take();
    until("fabric=stalled", || {
        status(&w).contains(" fabric=stalled ").then_some(())
    });
    let fs = fabric_status(&w);
    assert_eq!(kv(&fs, "state"), Some("stalled"), "{fs}");
    assert!(
        matches!(kv(&fs, "reason"), Some("closed" | "no-socket" | "refused")),
        "{fs}"
    );
    assert_eq!(kv(&fs, "rtt_ms"), Some(rtt.to_string().as_str()), "{fs}");
    let s = status(&w);
    assert!(s.contains(&format!(" fabric_rtt_ms={rtt} ")), "{s}");

    const WAIT: Duration = Duration::from_secs(300);
    let asked = Instant::now();
    let reply = w
        .verb(&format!(
            "@{sid} post to=@{sid} kind=note --wait={} still there?",
            WAIT.as_millis()
        ))
        .header()
        .to_string();
    let answered_in = asked.elapsed();
    assert!(
        reply.starts_with("ERR fabric stalled id=") && reply.ends_with(" queued=1"),
        "{reply}"
    );
    assert!(
        answered_in < harness::DEADLINE,
        "refused, not after the {WAIT:?} wait: {answered_in:?}"
    );
    let post_id: u64 = reply
        .split_whitespace()
        .find_map(|t| t.strip_prefix("id="))
        .and_then(|n| n.parse().ok())
        .expect("the refusal names the post id");
    assert!(
        w.verb(&format!("@{sid} inbox --peek"))
            .rows()
            .iter()
            .any(|r| r.starts_with(&format!("post {post_id} "))),
        "the refused post is still queued"
    );

    let broker = Broker::open_guarded(&w.broker_log, SECRET.to_vec()).expect("reopen the log");
    let handle = broker
        .serve(&w.broker_sock)
        .expect("serve the same socket again");
    w.broker = Some(broker);
    w.handle = Some(handle);
    until("fabric=connected after the restart", || {
        status(&w).contains(" fabric=connected ").then_some(())
    });
    let landed_by = Instant::now() + harness::DEADLINE;
    loop {
        if w.inbox(&sid)
            .iter()
            .any(|r| r.contains(" kind=note ") && r.contains("text=still%20there?"))
        {
            break;
        }
        assert!(
            Instant::now() < landed_by,
            "the queued post never landed after the reconnect\n  inbox: {:#?}\n  ev: {:#?}\n  \
             fabric status: {}\n  log tail:\n{}",
            w.verb(&format!("@{sid} inbox --peek")).rows(),
            w.ev(),
            fabric_status(&w),
            w.log_tail()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    let s = status(&w);
    assert!(s.contains(" fabric=connected "), "{s}");
    assert!(
        kv(&s, "fabric_rtt_ms").is_some_and(|n| n.parse::<u64>().is_ok_and(|n| n >= 1)),
        "{s}"
    );
}

/// `connected`.**
#[test]
fn a_broker_path_nothing_listens_on_is_stalled_and_never_connected() {
    let nowhere = format!("/tmp/atl-r13-nowhere-{}.sock", std::process::id());
    let _ = std::fs::remove_file(&nowhere);
    let w = World::boot_at("r13-nosock", &[], &[], Some(&nowhere));
    let sid = boot_sid(&w);

    let first = until("the bridge to attach", || {
        let fs = fabric_status(&w);
        (kv(&fs, "state") == Some("stalled")).then_some(fs)
    });
    assert!(
        matches!(kv(&first, "reason"), Some("starting" | "no-socket")),
        "attached is stalled until the bridge reports, then the dial's verdict: {first}"
    );
    let fs = until("reason=no-socket", || {
        let fs = fabric_status(&w);
        (kv(&fs, "reason") == Some("no-socket")).then_some(fs)
    });
    assert_eq!(kv(&fs, "state"), Some("stalled"), "{fs}");
    assert_eq!(kv(&fs, "rtt_ms"), Some("-"), "no ack ever: {fs}");
    assert_eq!(kv(&fs, "link_age_ms"), Some("-"), "{fs}");
    let reply = w
        .verb(&format!("@{sid} post to=@{sid} kind=ask anyone?"))
        .header()
        .to_string();
    assert_eq!(
        reply, "ERR fabric stalled id=1 queued=1",
        "an `ask` waits by default"
    );
}

/// life of the process, reporting nothing; the endpoint said `connected`.
#[test]
fn a_broker_that_accepts_but_never_acks_is_stalled_no_ack() {
    let dir = format!("/tmp/atl-stub13-{}", std::process::id());
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("stub dir");
    let stub = format!("{dir}/s.sock");
    let listener = UnixListener::bind(&stub).expect("bind the stub");
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            std::thread::spawn(move || {
                let mut sink = [0u8; 4096];
                while stream.read(&mut sink).is_ok_and(|n| n > 0) {}
            });
        }
    });

    let w = World::boot_at(
        "r13-stub",
        &[],
        &[("ATERM_LINK_ACK_DEADLINE_MS", "250")],
        Some(&stub),
    );
    until("the bridge to attach", || {
        (kv(&fabric_status(&w), "state") == Some("stalled")).then_some(())
    });
    let fs = until("stalled reason=no-ack", || {
        let fs = fabric_status(&w);
        (kv(&fs, "reason") == Some("no-ack")).then_some(fs)
    });
    assert_eq!(kv(&fs, "state"), Some("stalled"), "{fs}");
    assert_eq!(kv(&fs, "rtt_ms"), Some("-"), "{fs}");
    let s = status(&w);
    assert!(
        s.contains(" fabric=stalled ")
            && kv(&s, "fabric_rtt_ms") == Some("-")
            && kv(&s, "fabric_link_age_ms") == Some("-"),
        "never connected: {s}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// a `post --wait` refused at once.
#[test]
fn review_a_bridge_that_cannot_finish_its_attach_never_reads_connected() {
    let fleet_face = format!(":/f/{FLEET}/fleet/>");
    let w = World::boot_with_grants("r13-nofleet", &|node| {
        node_grants(node)
            .into_iter()
            .filter(|g| !g.ends_with(&fleet_face))
            .collect()
    });
    let sid = boot_sid(&w);
    let booted = Instant::now();
    let stalled_in = until("fabric=stalled reason=read", || {
        let fs = fabric_status(&w);
        (kv(&fs, "state") == Some("stalled") && kv(&fs, "reason") == Some("read"))
            .then(|| booted.elapsed())
    });
    eprintln!(
        "MEASURED refused halt read -> stalled reason=read: {} ms after boot",
        stalled_in.as_millis()
    );
    let mut god = w.god();
    let (rows, _) = god
        .last(&format!("/f/{FLEET}/pub/{}/>", w.node), "", 8)
        .expect("a Last walk of the node's face");
    assert!(
        rows.is_empty(),
        "an attach that never finished must not have published presence: {rows:?}"
    );
    let posted = w.verb(&format!("@{sid} post to=@{sid} kind=note --wait=2000 hi"));
    assert!(
        posted.header().starts_with("ERR fabric stalled"),
        "a wait under a reported stall fails fast: {}",
        posted.header()
    );
}

/// ring until it is killed, and the wait below fails at its hang detector.
#[test]
fn a_renewed_cap_is_attached_at_the_next_redial_without_a_restart() {
    let w = World::boot_with_grants("cap-renew", &|node| {
        node_grants(node)
            .into_iter()
            .map(|g| g.replacen(':', ",exp=1:", 1))
            .collect()
    });
    until("stalled reason=expired under the lapsed ring", || {
        let fs = fabric_status(&w);
        (kv(&fs, "state") == Some("stalled") && kv(&fs, "reason") == Some("expired")).then_some(())
    });
    let pid = w.bridge_pid();
    let renewed: String = node_grants(&w.node).iter().map(|g| cap_line(g)).collect();
    let staged = w.tmp.join("node.cap.renewed");
    std::fs::write(&staged, renewed).expect("the renewed ring");
    std::fs::rename(&staged, w.tmp.join("node.cap")).expect("install it over node.cap");
    w.wait_ready();
    assert_eq!(
        w.bridge_pid(),
        pid,
        "the bridge that held the lapsed ring attached the renewed one"
    );
}
