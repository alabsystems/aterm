// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Broker link-state reporting and reconnect tests against a live headless instance.

use crate::harness;
use crate::retention_live::{link_moves_since, node_log_cursor};

use std::os::unix::net::UnixListener;
use std::time::{Duration, Instant};

use astream_broker::Broker;
use harness::{FLEET, SECRET, World, cap_line, kv, until};

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

/// **A KILLED BRIDGE'S LOSS LANDS AHEAD OF THE NEXT ATTACH, HOWEVER LONG ITS LANES TAKE**: the
/// real lanes and their guard under the supervisor's own loop, each lane held past its hang-up
/// until the replacement is attached, read off the node log's link marks: the killed
/// generation's `state=disconnected`, then the next one's `state=stalled`.
///
/// The supervisor tells the loss itself at the bridge's exit, waiting on no lane: one that left
/// the loss to the lanes alone attaches the next bridge during the hold, and the loss lands
/// behind it. This asserts no time: that a relaunch waits on no lane is `fabric_launch`'s
/// `a_relaunch_waits_on_no_old_lane_and_the_old_ones_late_loss_holds_no_session`, and that a
/// healthy run's relaunch waits nothing is its
/// `the_loop_waits_nothing_after_a_healthy_run_and_the_floor_after_a_young_death`.
#[test]
fn a_killed_bridges_loss_lands_ahead_of_the_next_attach_however_long_its_lanes_take() {
    let hold = harness::Hold::new("relaunch-order");
    let w = World::boot_with(
        "relaunch-order",
        &[],
        &[("ATERM_TEST_PUSH_HOLD", &hold.env())],
    );
    w.wait_ready();
    let cursor = node_log_cursor(&w);
    let killed = w.bridge_pid();
    hold.on("lane-loss");
    harness::kill(killed, 9);
    // The old lanes cannot finish before this barrier: a supervisor that waits for them
    // cannot attach its replacement, regardless of its back-off or the machine's speed.
    w.await_bridge_back(killed);
    hold.off("lane-loss");

    let moves = link_moves_since(&w, &cursor);
    let said: Vec<(&str, u64)> = moves
        .iter()
        .take(2)
        .map(|row| {
            let generation = kv(row, "gen").and_then(|g| g.parse().ok());
            (
                kv(row, "state").unwrap_or_else(|| panic!("no state= in {row}")),
                generation.unwrap_or_else(|| panic!("no gen= in {row}")),
            )
        })
        .collect();
    assert!(
        matches!(
            said[..],
            [("disconnected", killed), ("stalled", next)] if killed < next
        ),
        "the link over a relaunch ({cursor}): {moves:#?}\n{}",
        w.log_tail()
    );
}

/// The broker is the harness's own, so "killed" is its handle's `shutdown` —
/// which force-closes every live connection, exactly what a `kill -9` of a
/// `link broker` process does to its peers — and "restarted" is a new `Broker`
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
    // A live link files a fresh `link up rtt=` at least every LINK_REFRESH of acks, which can land
    // between the two verbs, so here each carries its own reading; that they read the one link
    // fact is held below, on the stall, where no ack can move it.
    let live: u64 = kv(&fs, "rtt_ms")
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("connected must carry a numeric rtt_ms=: {fs}"));
    assert!(live >= 1, "a real ack never reads 0 ms: {fs}");
    let lane = aterm_types::control_verbs::BRIDGE_LANE.to_string();
    assert_eq!(kv(&fs, "bridge_lane"), Some(lane.as_str()), "{fs}");
    assert!(kv(&fs, "build").is_some(), "{fs}");
    assert_eq!(kv(&fs, "bridge_build"), kv(&fs, "build"), "{fs}");
    assert_eq!(kv(&fs, "lane_faults"), Some("0"), "{fs}");

    if let Some(mut h) = w.handle.take() {
        h.shutdown();
    }
    w.broker.take();
    // The shutdown wakes the broker's subscribers before it closes their connections, so the
    // bridge can first report a cut subscription the broker still answers for (`cut-fleet`,
    // `cut-inbox`, `cut-say`); its redial's verdict replaces that.
    let fs = until("fabric=stalled on a redial's verdict", || {
        let fs = fabric_status(&w);
        (kv(&fs, "state") == Some("stalled")
            && matches!(kv(&fs, "reason"), Some("closed" | "no-socket" | "refused")))
        .then_some(fs)
    });
    // The stall keeps the last ack's rtt as history: the first one read, or a probe's the bridge
    // was answered during the shutdown. Nothing acks a stalled link, so `status` must read the
    // same rtt `fabric status` does: the two verbs report one link fact.
    let kept: u64 = kv(&fs, "rtt_ms")
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("a stall keeps the last rtt: {fs}"));
    assert!(kept >= 1, "{fs}");
    let s = status(&w);
    assert!(s.contains(" fabric=stalled "), "{s}");
    assert!(s.contains(&format!(" fabric_rtt_ms={kept} ")), "{s}");

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

/// **A SOCKET PATH NOTHING LISTENS ON IS `stalled reason=no-socket`, AND NEVER
/// `connected`.**
#[test]
fn a_broker_path_nothing_listens_on_is_stalled_and_never_connected() {
    let dir = harness::sock_dir("r13-nosock");
    let nowhere = dir
        .path()
        .join("nowhere.sock")
        .to_string_lossy()
        .into_owned();
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
    let dir = harness::sock_dir("stub13");
    let stub = dir.path().join("s.sock").to_string_lossy().into_owned();
    let listener = UnixListener::bind(&stub).expect("bind the stub");
    listener.set_nonblocking(true).unwrap();
    let (stop, stopping) = std::sync::mpsc::channel();
    let (accepted, acceptance) = std::sync::mpsc::channel();
    struct Stub(
        Option<std::sync::mpsc::Sender<()>>,
        Option<std::thread::JoinHandle<()>>,
    );
    impl Drop for Stub {
        fn drop(&mut self) {
            drop(self.0.take());
            self.1.take().unwrap().join().unwrap();
        }
    }
    let worker = std::thread::spawn(move || {
        let mut streams = Vec::new();
        loop {
            match listener.accept() {
                Ok((stream, _)) => {
                    streams.push(stream);
                    let _ = accepted.send(());
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(e) => panic!("accept the bridge: {e}"),
            }
            if !matches!(
                stopping.recv_timeout(Duration::from_millis(20)),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout)
            ) {
                break;
            }
        }
    });
    let _stub = Stub(Some(stop), Some(worker));

    // This fixture requires a timeout, never a successful answer. Its short injected
    // deadline exercises the same socket timeout; every event wait keeps the minute detector.
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
    acceptance
        .recv_timeout(harness::DEADLINE)
        .expect("the silent broker accepted a real connection");
    assert_eq!(kv(&fs, "state"), Some("stalled"), "{fs}");
    assert_eq!(kv(&fs, "rtt_ms"), Some("-"), "{fs}");
    let s = status(&w);
    assert!(
        s.contains(" fabric=stalled ")
            && kv(&s, "fabric_rtt_ms") == Some("-")
            && kv(&s, "fabric_link_age_ms") == Some("-"),
        "never connected: {s}"
    );
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
        astream_cap::GrantCeiling::aterm_node_ring(FLEET, node)
            .expect("a node ring")
            .rules()
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
        astream_cap::GrantCeiling::aterm_node_ring(FLEET, node)
            .expect("a node ring")
            .rules()
            .into_iter()
            .map(|g| g.replacen(':', ",exp=1:", 1))
            .collect()
    });
    until("stalled reason=expired under the lapsed ring", || {
        let fs = fabric_status(&w);
        (kv(&fs, "state") == Some("stalled") && kv(&fs, "reason") == Some("expired")).then_some(())
    });
    let pid = w.bridge_pid();
    let renewed: String = astream_cap::GrantCeiling::aterm_node_ring(FLEET, &w.node)
        .expect("a node ring")
        .rules()
        .iter()
        .map(|g| cap_line(g))
        .collect();
    let staged = w.tmp.join("node.cap.renewed");
    harness::write_cap(&staged, renewed);
    std::fs::rename(&staged, w.tmp.join("node.cap")).expect("install it over node.cap");
    w.wait_ready();
    assert_eq!(
        w.bridge_pid(),
        pid,
        "the bridge that held the lapsed ring attached the renewed one"
    );
}
