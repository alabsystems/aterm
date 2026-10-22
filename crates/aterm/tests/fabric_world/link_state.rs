// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **`fabric=` tells the truth about the BROKER LINK, without a heartbeat.**
//!
//! The bridge owns the broker link — the dial, every ack — and reports it to
//! the endpoint over its own verb lane (`link up rtt=` / `link down reason=`)
//! on change and on a 2x move of the round trip, never on a clock.
//!
//! Four worlds, each against a REAL headless aterm and a REAL bridge child:
//!
//! * a broker that is killed under a connected bridge → `stalled`, a parked
//!   `post --wait` refused without sitting out its wait, and `connected` again
//!   when the broker is restarted on the same socket — with the queued post
//!   delivered by the reconnected bridge;
//! * a `--broker` path nothing listens on → `stalled reason=no-socket`, and
//!   never `connected`;
//! * a stub that ACCEPTS the connection and never acks → `stalled
//!   reason=no-ack`, never `connected`.
//!
//! Every wait is an `until` over what the instance PUBLISHES; the measured
//! numbers are printed so a run's output is the evidence.

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

/// **A KILLED BROKER IS `stalled`, A PARKED WAIT IS REFUSED, AND A RESTART IS
/// `connected` AGAIN.**
///
/// The broker is the harness's own, so "killed" is its handle's `shutdown` —
/// which force-closes every live connection, exactly what a `kill -9` of a
/// `link broker` process does to its peers — and "restarted" is a new `Broker`
/// on the same log and the same socket path.
#[test]
fn a_killed_broker_stalls_the_fabric_and_a_restart_reconnects() {
    let mut w = World::boot("r13-kill", &[]);
    w.wait_ready();
    let sid = boot_sid(&w);

    // CONNECTED CARRIES A ROUND TRIP. The first `link up` carries the
    // attach's last acked exchange (the bridge reports nothing `up` until its
    // attach completed), so the number is a real round trip on this machine —
    // printed as the measurement the commit records.
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
    // AND THE BRIDGE SAID WHAT IT IS: a real bridge's `hello` taken by a real
    // endpoint of the same tree — one lane, one build, no line refused.
    let lane = aterm_types::control_verbs::BRIDGE_LANE.to_string();
    assert_eq!(kv(&fs, "bridge_lane"), Some(lane.as_str()), "{fs}");
    assert!(kv(&fs, "build").is_some(), "{fs}");
    assert_eq!(kv(&fs, "bridge_build"), kv(&fs, "build"), "{fs}");
    assert_eq!(kv(&fs, "lane_faults"), Some("0"), "{fs}");

    // KILL IT.
    if let Some(mut h) = w.handle.take() {
        h.shutdown();
    }
    w.broker.take();
    until("fabric=stalled", || {
        status(&w).contains(" fabric=stalled ").then_some(())
    });
    // The reason is the reader's EOF or the redial that followed it — the
    // socket file is gone with the broker, so the redial says `no-socket`.
    let fs = fabric_status(&w);
    assert_eq!(kv(&fs, "state"), Some("stalled"), "{fs}");
    assert!(
        matches!(kv(&fs, "reason"), Some("closed" | "no-socket" | "refused")),
        "{fs}"
    );
    // The last rtt is history and stays; the age keeps growing.
    assert_eq!(kv(&fs, "rtt_ms"), Some(rtt.to_string().as_str()), "{fs}");
    let s = status(&w);
    assert!(s.contains(&format!(" fabric_rtt_ms={rtt} ")), "{s}");

    // A WAIT ON A STALLED FABRIC IS REFUSED, not sat out, and the post stays
    // queued. How promptly is aterm-gui's in-process
    // `the_link_is_the_bridges_to_report_and_stalled_refuses_a_wait_at_once`.
    // Here the wait asked is five minutes and the answer is held to the
    // harness's DEADLINE, a minute's hang detector (AGENTS.md): only a wait
    // that was sat out reaches it.
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

    // RESTART IT on the same socket, and the bridge comes back on its own.
    let broker = Broker::open_guarded(&w.broker_log, SECRET.to_vec()).expect("reopen the log");
    let handle = broker
        .serve(&w.broker_sock)
        .expect("serve the same socket again");
    w.broker = Some(broker);
    w.handle = Some(handle);
    until("fabric=connected after the restart", || {
        status(&w).contains(" fabric=connected ").then_some(())
    });
    // ...and the post it refused to wait on is delivered by the reconnected
    // bridge: to itself, through the broker, into its own inbox. (A hang
    // detector: the loop leaves the moment the row lands.)
    let landed_by = Instant::now() + harness::DEADLINE;
    loop {
        // The endpoint pct-encodes the space and leaves `?` as it is.
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

/// **A SOCKET PATH NOTHING LISTENS ON IS `stalled reason=no-socket`, AND NEVER
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

/// **A BROKER THAT ACCEPTS AND NEVER ACKS IS `stalled reason=no-ack`.** Before
/// the ack deadline existed this bridge parked in its first `attach` for the
/// life of the process, reporting nothing; the endpoint said `connected`.
#[test]
fn a_broker_that_accepts_but_never_acks_is_stalled_no_ack() {
    // NOT under the world's own scratch: that directory is made as the world
    // boots, and the stub must be listening before then, or the bridge's dial
    // reads `no-socket`, a different test.
    let dir = format!("/tmp/atl-stub13-{}", std::process::id());
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("stub dir");
    let stub = format!("{dir}/s.sock");
    let listener = UnixListener::bind(&stub).expect("bind the stub");
    // Accept everything, read everything, answer nothing.
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            std::thread::spawn(move || {
                let mut sink = [0u8; 4096];
                while stream.read(&mut sink).is_ok_and(|n| n > 0) {}
            });
        }
    });

    // The verdict is the deadline PASSING, so its length decides nothing here:
    // a quarter second (`aterm-link`'s `ACK_DEADLINE_SEAM`), not the production
    // five the wait below used to sit out.
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

/// **A BRIDGE THAT CANNOT FINISH ITS ATTACH NEVER READS `connected`, AND
/// WRITES NO RECORD.** Its cap is the node ring minus `ro:/f/<F>/fleet/>`, so
/// the broker refuses the standing-halt read the bridge cannot attach without.
/// The halt is read before anything is published and no ack is reported `up`
/// until the attach completed: `stalled reason=read`, an empty node face, and
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
    // A hang detector (nothing bounds `stalled_in`; it is printed as the
    // measurement), so the harness's minute rather than one back-off tick.
    let stalled_in = until("fabric=stalled reason=read", || {
        let fs = fabric_status(&w);
        (kv(&fs, "state") == Some("stalled") && kv(&fs, "reason") == Some("read"))
            .then(|| booted.elapsed())
    });
    eprintln!(
        "MEASURED refused halt read -> stalled reason=read: {} ms after boot",
        stalled_in.as_millis()
    );
    // It never published anything — not even the first attempt's presence:
    // the node's face on the bus is empty.
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

/// **A RENEWED CAP IS ATTACHED AT THE NEXT REDIAL, BY THE SAME BRIDGE**
/// (audit 16). The node's ring is minted already lapsed (`exp=1`, 1970), so
/// the guarded broker refuses its attach AS EXPIRED and the bridge reads
/// `stalled reason=expired` — not the `attach` of a cap the broker does not
/// take at all, whose remedy is another mint, not a renewal — redialling
/// under its back-off. A renewed ring written over
/// `node.cap` by rename — as `aterm fabric on` installs a re-minted one — is
/// all it takes: the bridge reads the file again before its next dial and
/// comes up `connected`, with the same pid. A bridge that read its cap only
/// at start (the negative control: the code before this) redials the lapsed
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
