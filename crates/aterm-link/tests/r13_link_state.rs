// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **ROUND 13 B — `fabric=` tells the truth about the BROKER LINK, without a
//! heartbeat.**
//!
//! Until this round `fabric=connected` meant "a bridge process is attached":
//! measured 2026-09-12, a bridge pointed at a socket nothing served reported
//! `connected` for as long as it lived, and killing the broker moved nothing.
//! The bridge owns the broker link — the dial, every ack — and now reports it
//! to the endpoint over its own verb lane (`link up rtt=` / `link down
//! reason=`) on change and on a 2x move of the round trip, never on a clock.
//!
//! Four worlds, each against a REAL headless aterm and a REAL bridge child:
//!
//! * a broker that is killed under a connected bridge → `stalled` within the
//!   bridge's reconnect back-off tick, a parked `post --wait` answered at once,
//!   and `connected` again when the broker is restarted on the same socket —
//!   with the queued post delivered by the reconnected bridge;
//! * a `--broker` path nothing listens on → `stalled reason=no-socket`, and
//!   never `connected`;
//! * a stub that ACCEPTS the connection and never acks → `stalled
//!   reason=no-ack` after the bridge's 5 s ack deadline, never `connected`.
//!
//! Every wait is an `until` over what the instance PUBLISHES; the measured
//! numbers are printed so a run's output is the evidence.

mod harness;

use std::io::Read as _;
use std::os::unix::net::UnixListener;
use std::time::{Duration, Instant};

use astream_broker::Broker;
use harness::{node_grants, until, until_within, World, FLEET, SECRET};

/// `RECONNECT_MAX` in `bridge.rs`: the widest gap between two redials, and so
/// the widest gap between a broker's death being noticed and the endpoint
/// hearing about it (the notice itself is the reader's EOF, which is immediate).
const BACKOFF_TICK: Duration = Duration::from_secs(5);

/// `ACK_DEADLINE` in `bridge.rs`.
const ACK_DEADLINE: Duration = Duration::from_secs(5);

/// The `key=` value of a whitespace-token header.
fn kv<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    line.split_whitespace()
        .find_map(|t| t.strip_prefix(key).and_then(|r| r.strip_prefix('=')))
}

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

/// **A KILLED BROKER IS `stalled` WITHIN A BACK-OFF TICK, A PARKED WAIT IS
/// ANSWERED AT ONCE, AND A RESTART IS `connected` AGAIN.**
///
/// The broker is the harness's own, so "killed" is its handle's `shutdown` —
/// which force-closes every live connection, exactly what a `kill -9` of a
/// `link broker` process does to its peers — and "restarted" is a new `Broker`
/// on the same log and the same socket path.
#[test]
fn a_killed_broker_stalls_the_fabric_within_a_backoff_tick_and_a_restart_reconnects() {
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

    // KILL IT.
    let killed_at = Instant::now();
    if let Some(mut h) = w.handle.take() {
        h.shutdown();
    }
    w.broker.take();
    let stalled_in = until_within(
        BACKOFF_TICK + Duration::from_secs(2),
        "fabric=stalled",
        || {
            status(&w)
                .contains(" fabric=stalled ")
                .then(|| killed_at.elapsed())
        },
    );
    eprintln!(
        "MEASURED killed broker -> fabric=stalled: {} ms",
        stalled_in.as_millis()
    );
    assert!(
        stalled_in <= BACKOFF_TICK,
        "stalled must land within one reconnect back-off tick: {stalled_in:?}"
    );
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

    // A WAIT ON A STALLED FABRIC IS REFUSED AT ONCE, and the post stays queued.
    let asked = Instant::now();
    let reply = w
        .verb(&format!(
            "@{sid} post to=@{sid} kind=note --wait=30000 still there?"
        ))
        .header()
        .to_string();
    let answered_in = asked.elapsed();
    assert!(
        reply.starts_with("ERR fabric stalled id=") && reply.ends_with(" queued=1"),
        "{reply}"
    );
    assert!(
        answered_in < Duration::from_secs(5),
        "refused at once, not after the 30 s wait: {answered_in:?}"
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

    // RESTART IT on the same socket, and the bridge comes back on its own —
    // within one back-off tick plus its attach.
    let restarted_at = Instant::now();
    let broker = Broker::open_guarded(&w.broker_log, SECRET.to_vec()).expect("reopen the log");
    let handle = broker
        .serve(&w.broker_sock)
        .expect("serve the same socket again");
    w.broker = Some(broker);
    w.handle = Some(handle);
    let connected_in = until_within(
        BACKOFF_TICK + Duration::from_secs(10),
        "fabric=connected after the restart",
        || {
            status(&w)
                .contains(" fabric=connected ")
                .then(|| restarted_at.elapsed())
        },
    );
    eprintln!(
        "MEASURED restarted broker -> fabric=connected: {} ms",
        connected_in.as_millis()
    );
    assert!(
        connected_in <= BACKOFF_TICK + Duration::from_secs(5),
        "reconnect within a back-off tick and an attach: {connected_in:?}"
    );
    // ...and the post it refused to wait on is delivered by the reconnected
    // bridge: to itself, through the broker, into its own inbox.
    let landed_by = Instant::now() + Duration::from_secs(20);
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
/// `connected`.** The case that used to read `connected` for the life of the
/// process.
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

    // NEVER CONNECTED: watched across several redials (the back-off climbs to
    // 5 s, so three seconds cover a handful of them).
    let watch_until = Instant::now() + Duration::from_secs(3);
    while Instant::now() < watch_until {
        let s = status(&w);
        assert!(
            s.contains(" fabric=stalled ") && s.ends_with(" fabric_rtt_ms=- fabric_link_age_ms=-"),
            "{s}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    let reply = w
        .verb(&format!("@{sid} post to=@{sid} kind=ask anyone?"))
        .header()
        .to_string();
    assert_eq!(
        reply, "ERR fabric stalled id=1 queued=1",
        "an `ask` waits by default"
    );
}

/// **A BROKER THAT ACCEPTS AND NEVER ACKS IS `stalled reason=no-ack` AFTER THE
/// ACK DEADLINE.** Before the deadline existed this bridge parked in its first
/// `attach` for the life of the process, reporting nothing; the endpoint said
/// `connected`.
#[test]
fn a_broker_that_accepts_but_never_acks_is_stalled_after_the_ack_deadline() {
    // NOT under the world's own `/tmp/atl-<tag>-<pid>`: the harness wipes that
    // directory as it boots, and a stub socket inside it is gone before the
    // bridge dials — which reads as `no-socket`, a different test.
    let dir = format!("/tmp/atl-stub13-{}", std::process::id());
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("stub dir");
    let stub = format!("{dir}/s.sock");
    let listener = UnixListener::bind(&stub).expect("bind the stub");
    // Accept everything, read everything, answer nothing — and say WHEN the
    // first accept happened: the bridge starts its ack deadline the moment it
    // connects (bridge.rs, right after the dial), so the stub's accept is the
    // deadline's true origin. The first poll that sees `state=stalled` trails
    // it by however long the harness took to boot and answer `fabric_status`;
    // on a loaded 4-core Intel Mac that lag was 1.149 s (2026-09-15 gate), one
    // slack second short of the assertion below, while the bridge itself had
    // honoured its 5 s deadline exactly.
    let (accepted_tx, accepted_rx) = std::sync::mpsc::channel::<Instant>();
    std::thread::spawn(move || {
        let mut first = Some(accepted_tx);
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            if let Some(tx) = first.take() {
                let _ = tx.send(Instant::now());
            }
            std::thread::spawn(move || {
                let mut sink = [0u8; 4096];
                while stream.read(&mut sink).is_ok_and(|n| n > 0) {}
            });
        }
    });

    let w = World::boot_at("r13-stub", &[], &[], Some(&stub));
    let seen_at = until("the bridge to attach", || {
        let fs = fabric_status(&w);
        (kv(&fs, "state") == Some("stalled")).then(Instant::now)
    });
    // The accept precedes the attach the poll saw, so it is already in the
    // channel; measure from it, and record how far behind it the poll was.
    let attached_at = accepted_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("the stub accepted the bridge's dial before the attach was seen");
    eprintln!(
        "MEASURED the first poll saw the attach {} ms after the stub accepted",
        seen_at.saturating_duration_since(attached_at).as_millis()
    );
    let no_ack_by = attached_at + ACK_DEADLINE + Duration::from_secs(10);
    let fs = loop {
        let fs = fabric_status(&w);
        if kv(&fs, "reason") == Some("no-ack") {
            break fs;
        }
        assert!(
            Instant::now() < no_ack_by,
            "reason=no-ack never arrived; last: {fs}\n  log tail:\n{}",
            w.log_tail()
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    let waited = attached_at.elapsed();
    eprintln!(
        "MEASURED stub that never acks -> stalled reason=no-ack: {} ms after the stub accepted",
        waited.as_millis()
    );
    // The deadline's clock started at the bridge's connect, which the stub's
    // accept trails by a scheduler hop at most, so the lower bound keeps a
    // second of slack purely as margin; the upper bound is the deadline plus
    // one redial's worth of back-off.
    assert!(
        waited >= ACK_DEADLINE - Duration::from_secs(1),
        "no-ack must wait out the ack deadline: {waited:?}"
    );
    assert!(
        waited <= ACK_DEADLINE + Duration::from_secs(6),
        "no-ack must land at the deadline, not long after: {waited:?}"
    );
    assert_eq!(kv(&fs, "state"), Some("stalled"), "{fs}");
    assert_eq!(kv(&fs, "rtt_ms"), Some("-"), "{fs}");
    let s = status(&w);
    assert!(
        s.contains(" fabric=stalled ") && s.ends_with(" fabric_rtt_ms=- fabric_link_age_ms=-"),
        "never connected: {s}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// **A BRIDGE THAT CANNOT FINISH ITS ATTACH NEVER READS `connected`, AND
/// WRITES NO RECORD** — the round-13 review's finding 5. Its cap is the node
/// ring minus `ro:/f/<F>/fleet/>`, so the broker refuses the standing-halt
/// read the bridge cannot attach without. Before: the presence publish was
/// acked first (`link up`), the halt read refused (`link down reason=read`),
/// the connection dropped and redialed — `fabric=` flapping and a node+session
/// presence republish every back-off tick, measured at twenty bus records in
/// 8 s. Now the halt is read before anything is published and no ack is
/// reported `up` until the attach completed: a steady `stalled reason=read`,
/// a bus head that does not move, and a `post --wait` refused at once.
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
    let stalled_in = until_within(
        BACKOFF_TICK + Duration::from_secs(2),
        "fabric=stalled reason=read",
        || {
            let fs = fabric_status(&w);
            (kv(&fs, "state") == Some("stalled") && kv(&fs, "reason") == Some("read"))
                .then(|| booted.elapsed())
        },
    );
    eprintln!(
        "MEASURED refused halt read -> stalled reason=read: {} ms after boot",
        stalled_in.as_millis()
    );
    // The broker's head before the window: nothing was ever published by
    // this bridge, and nothing will be.
    let mut god = w.god();
    let head_of = |god: &mut astream_broker::Client| -> u64 {
        let (_, (_, head)) = god
            .fetch(0, &format!("/f/{FLEET}/>"), 0)
            .expect("the head query");
        head
    };
    let head_before = head_of(&mut god);
    let window = Duration::from_secs(8);
    let started = Instant::now();
    let mut states: Vec<String> = Vec::new();
    while started.elapsed() < window {
        let fs = fabric_status(&w);
        let state = kv(&fs, "state").unwrap_or("-").to_string();
        let reason = kv(&fs, "reason").unwrap_or("-").to_string();
        assert_ne!(state, "connected", "never connected: {fs}");
        let sample = format!("{state}/{reason}");
        if states.last() != Some(&sample) {
            states.push(sample);
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    let head_after = head_of(&mut god);
    eprintln!(
        "MEASURED link states over {window:?}: {states:?}; bus records appended in the window: {}",
        head_after - head_before
    );
    assert_eq!(
        states,
        vec!["stalled/read".to_string()],
        "one steady state, not a flap: {states:?}"
    );
    assert_eq!(
        head_after, head_before,
        "an attach that cannot finish publishes nothing (§7: no record for no new information)"
    );
    // And it never published anything — not even the first attempt's
    // presence: the node's face on the bus is empty.
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
