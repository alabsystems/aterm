// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE BUS MIRROR END TO END (`docs/DESIGN-session-log-2026-10-15.md` §5.5,
//! session log phase 4): a real headless aterm, its real bridge and a guarded
//! broker. Off by default it writes nothing anywhere; with `--log turns`
//! every session's bus-class records reach its `ev` face, each `(ep, seq)`
//! once in the reader's view (ruling Q0), `ms` dense, with one exact cover
//! per hole an eviction made (ruling Q2), through a kill at each step of a
//! batch and a broker restart with a pin outstanding (ruling Q12), no host
//! or content text crossing, and `aterm fabric brief` folding what the bus
//! holds. Every wait is a one-minute hang detector.

use std::collections::{BTreeMap, BTreeSet};

use aterm_slog::bus::BusSpace;

use super::harness::{FLEET, World, until};

/// A world whose bridge mirrors (`serve --log turns`), ready.
fn turns(tag: &str, env: &[(&str, &str)]) -> World {
    let w = World::boot_flags_with(tag, &["--log", "turns"], env);
    w.wait_ready();
    w
}

/// Every body on `face`, oldest first.
fn bodies(w: &World, face: &str) -> Vec<String> {
    w.read(|c| {
        let mut out = Vec::new();
        let mut from = 0u64;
        loop {
            let (rows, (next, head)) = c.fetch(from, face, 256)?;
            for (_, _, body) in rows {
                out.push(String::from_utf8_lossy(&body).into_owned());
            }
            if next >= head || next <= from {
                return Ok(out);
            }
            from = next;
        }
    })
    .unwrap_or_else(|e| panic!("read {face}: {e}"))
}

/// A session's mirror as a reader holds it: the view of each seq space, and
/// how many bodies the bus holds of each `(ep, seq)` (a raw duplicate count:
/// a rate, never a law, ruling Q0).
struct Mirrored {
    spaces: BTreeMap<u32, BusSpace>,
    raw: BTreeMap<(u32, u64), usize>,
    v2: usize,
}

fn mirrored(w: &World, sid: &str) -> Mirrored {
    let face = format!("/f/{FLEET}/pub/{}/{sid}/ev", w.node);
    let mut m = Mirrored {
        spaces: BTreeMap::new(),
        raw: BTreeMap::new(),
        v2: 0,
    };
    for body in bodies(w, &face) {
        let Ok(b) = aterm_slog::decode_bus(&body) else {
            continue;
        };
        m.v2 += 1;
        if b.rec.op != aterm_slog::Op::Snap && b.rec.f.get_u(aterm_slog::Key::N).is_none() {
            *m.raw.entry((b.ep, b.rec.seq)).or_default() += 1;
        }
        m.spaces
            .entry(b.ep)
            .or_insert_with(|| BusSpace::new(b.ep))
            .offer(b);
    }
    m
}

/// The session's live turn-class seqs, as its own `log class=turn` lists them.
fn turn_class(w: &World, sid: &str) -> BTreeSet<u64> {
    let r = w.verb(&format!("@{sid} log class=turn"));
    assert!(r.ok(), "log: {}", r.header());
    r.rows()
        .iter()
        .filter_map(|row| row.split_whitespace().next()?.parse().ok())
        .collect()
}

/// QUIESCENT AND COMPLETE (QuiescentMirrorComplete): every live turn-class
/// record of the session is a record on the bus, and the newest space's `ms`
/// are dense.
fn complete(w: &World, sid: &str) -> Option<Mirrored> {
    let live = turn_class(w, sid);
    let m = mirrored(w, sid);
    let (_, space) = m.spaces.iter().next_back()?;
    let held: BTreeSet<u64> = space.records().map(|r| r.seq).collect();
    (space.holes().is_empty() && live.iter().all(|s| held.contains(s))).then_some(m)
}

/// A claim the session's log carries (a bus-class `mark`).
fn mark(w: &World, sid: &str, kind: &str) {
    let r = w.verb(&format!("@{sid} mark - {kind} word=review"));
    assert!(r.ok(), "mark {kind}: {}", r.header());
}

fn claims(w: &World, sid: &str, n: usize) {
    for i in 0..n {
        mark(w, sid, if i % 2 == 0 { "need" } else { "clear" });
    }
}

/// RULING Q5 END TO END: a bridge with the default `[fabric] log` mirrors
/// nothing. Sessions write claims and exit; the bus holds no `v=2` body on
/// any face, the state dir holds no `logpin`, and `fabric status` says
/// nothing of a mirror.
#[test]
fn the_default_mirrors_nothing_and_writes_nothing() {
    let w = World::boot("lm-off", &[]);
    w.wait_ready();
    let (a, b) = w.two_sessions();
    claims(&w, &a, 3);
    claims(&w, &b, 3);
    assert!(w.verb(&format!("@{b} close")).ok());
    // The bridge's own `ev` records still flow; give it a round to publish
    // whatever it would: a drain barrier through the same bridge.
    w.drained(&a);
    let all = bodies(&w, &format!("/f/{FLEET}/pub/>"));
    assert!(
        all.iter().all(|b| aterm_slog::decode_bus(b).is_err()),
        "a v=2 body on the bus with the mirror off: {:?}",
        all.iter().find(|b| b.starts_with("v=2"))
    );
    assert!(!w.state.join("logpin").exists(), "no pin is ever written");
    let fs = w.verb("fabric status");
    assert!(!fs.header().contains("bridge_log="), "{}", fs.header());
}

/// TURNS LAND ONCE, DENSE: two sessions' claims and the second's exit reach
/// their faces, each record once on the bus, every live turn-class record
/// there, `ms` dense, and `fabric status` says the mirror is on.
#[test]
fn turns_land_once_with_ms_dense() {
    let w = turns("lm-on", &[]);
    let (a, b) = w.two_sessions();
    claims(&w, &a, 6);
    claims(&w, &b, 4);
    for sid in [&a, &b] {
        let m = until("the session's mirror to be quiescent and complete", || {
            complete(&w, sid)
        });
        assert!(
            m.raw.values().all(|n| *n == 1),
            "a duplicate on the bus: {:?}",
            m.raw
        );
        let (_, space) = m.spaces.iter().next_back().expect("a space");
        assert_eq!(space.dup, 0);
        assert!(
            space.covers().is_empty(),
            "nothing was evicted: {:?}",
            space.covers()
        );
    }
    let fs = w.verb("fabric status");
    assert!(
        fs.header().contains(" bridge_log=turns mirror=on"),
        "{}",
        fs.header()
    );
    // The second session exits: its close reaches the bus too.
    assert!(w.verb(&format!("@{b} close")).ok());
    until("the closed session's close on the bus", || {
        let m = mirrored(&w, &b);
        let (_, space) = m.spaces.iter().next_back()?;
        let (fold, _) = space.fold();
        fold.state().closed.is_some().then_some(())
    });
    // RULING Q8: once its close has landed its `ev` face is retired — out of
    // the last-value index, its subject back in the node's budget — and its
    // records stay in the log, fetchable.
    let face = format!("/f/{FLEET}/pub/{}/{b}/ev", w.node);
    until("the closed session's ev face to be retired", || {
        let rows = w.read(|c| c.last(&face, "", 8)).ok()?.0;
        rows.iter().all(|(_, s, _)| *s != face).then_some(())
    });
    assert!(mirrored(&w, &b).v2 > 0, "the records stay");
}

/// RULING Q12's kill points: a bridge `SIGKILL`ed at `fault` comes back under
/// its supervisor, recovers its pin, and the reader's view holds every record
/// once and none missing.
fn a_kill_loses_and_doubles_nothing(tag: &str, fault: &str) {
    let w = turns(tag, &[("ATERM_LINK_FAULT", fault)]);
    let (a, _) = w.two_sessions();
    claims(&w, &a, 4);
    // One shot across restarts: the bridge that comes back under the
    // supervisor arms nothing, and completes what the killed one pinned.
    until("the fault to fire", || {
        w.state.join("fault-fired").exists().then_some(())
    });
    claims(&w, &a, 2);
    let m = until("the mirror to be complete after the kill", || {
        complete(&w, &a)
    });
    let (_, space) = m.spaces.iter().next_back().expect("a space");
    assert!(space.holes().is_empty(), "{fault}: {:?}", space.holes());
    let dups: usize = m.raw.values().map(|n| n - 1).sum();
    eprintln!(
        "MEASURED {fault}: {} bodies, {dups} raw duplicates on the bus",
        m.v2
    );
    assert_eq!(
        dups, 0,
        "{fault}: the recovery republished at its pinned seqs"
    );
}

#[test]
fn a_kill_after_the_pin_loses_and_doubles_nothing() {
    a_kill_loses_and_doubles_nothing("lm-k1", "kill-after-mirror-pin");
}

#[test]
fn a_kill_mid_batch_loses_and_doubles_nothing() {
    a_kill_loses_and_doubles_nothing("lm-k2", "kill-mid-mirror-batch");
}

#[test]
fn a_kill_before_the_landing_loses_and_doubles_nothing() {
    a_kill_loses_and_doubles_nothing("lm-k3", "kill-before-log-landed");
}

#[test]
fn a_kill_after_the_landing_loses_and_doubles_nothing() {
    a_kill_loses_and_doubles_nothing("lm-k4", "kill-after-log-landed");
}

/// RULING Q12: the broker restarts while a batch is pinned — the bridge held
/// there by its one-shot park, waited for on its marker, never raced — and
/// the reader's view holds every record once, none missing.
#[test]
fn a_broker_restart_with_a_pin_outstanding_doubles_nothing() {
    let mut w = turns(
        "lm-pk",
        &[("ATERM_LINK_FAULT", "park-after-mirror-pin-while-marked")],
    );
    let (a, _) = w.two_sessions();
    until("the first takes to land", || complete(&w, &a));
    let marker = w.state.join("park-mirror-pin");
    std::fs::write(&marker, b"1\n").expect("the park marker");
    claims(&w, &a, 3);
    until("the bridge to park with a pin outstanding", || {
        w.state.join("fault-fired").exists().then_some(())
    });
    let pin = std::fs::read_to_string(w.state.join("logpin")).expect("the pin");
    assert!(
        !pin.starts_with("logpin v=1 inc=0 n=0"),
        "a batch is pinned: {pin}"
    );
    // The broker goes away under the parked bridge (it cannot report the
    // link while parked, so this does not wait for it to notice).
    if let Some(mut h) = w.handle.take() {
        h.shutdown();
    }
    w.broker.take();
    let _ = std::fs::remove_file(&w.broker_sock);
    std::fs::remove_file(&marker).expect("release the park");
    w.start_broker();
    claims(&w, &a, 1);
    let m = until("the mirror to be complete after the restart", || {
        complete(&w, &a)
    });
    let dups: usize = m.raw.values().map(|n| n - 1).sum();
    eprintln!(
        "MEASURED broker restart with a pin out: {} bodies, {dups} raw duplicates",
        m.v2
    );
    let (_, space) = m.spaces.iter().next_back().expect("a space");
    assert_eq!(space.dup, 0);
    assert!(space.holes().is_empty());
}

/// RULING Q2: with the broker away, a small ring evicts the claims the mark
/// quota retires; once it is back, the bridge backfills each hole with ONE
/// exact cover — the covers disjoint, never two of one hole, and every `ms`
/// a record's or a cover's.
#[test]
fn absent_then_present_backfills_one_exact_cover_per_hole() {
    let mut w = turns("lm-ev", &[("ATERM_SLOG_RING", "24,65536")]);
    let (a, _) = w.two_sessions();
    until("the first takes to land", || complete(&w, &a));
    w.stop_broker();
    claims(&w, &a, 18);
    w.start_broker();
    let m = until("the backfill to complete", || complete(&w, &a));
    let (_, space) = m.spaces.iter().next_back().expect("a space");
    let covers = space.covers();
    assert!(
        !covers.is_empty(),
        "the quota retired claims before the take: {covers:?}"
    );
    let mut spans: Vec<(u64, u64)> = covers.iter().map(|(lo, n, _)| (*lo, lo + n - 1)).collect();
    spans.sort_unstable();
    for pair in spans.windows(2) {
        assert!(
            pair[0].1 + 1 < pair[1].0,
            "two covers of one hole, or overlapping: {spans:?}"
        );
    }
    let covered: u64 = covers.iter().map(|(_, n, _)| n).sum();
    let records = space.records_ms().filter(|(ms, _)| *ms > 0).count() as u64;
    assert_eq!(
        covered + records,
        space.max_ms(),
        "every ms once: a record's or one cover's"
    );
    assert!(covers.iter().all(|c| c.2 == "evicted"));
    eprintln!(
        "MEASURED backfill: {} covers, n= summing to {covered}, {records} records",
        covers.len()
    );
}

/// No host or content text crosses: a note's text, an OSC title, a command
/// line and the cwd stay local; no mail party, kind or offset key is on any
/// body.
#[test]
fn no_host_or_content_text_reaches_the_bus() {
    let w = turns("lm-leak", &[]);
    let (a, _) = w.two_sessions();
    assert!(
        w.verb(&format!("@{a} mark - note text=SECRET-NOTE-TEXT"))
            .ok()
    );
    assert!(
        w.verb(&format!(
            "@{a} send printf '\\033]0;TOPSECRET-TITLE\\007'; echo SECRET-COMMAND"
        ))
        .ok()
    );
    assert!(w.verb(&format!("@{a} key enter")).ok());
    claims(&w, &a, 2);
    until("the mirror to be complete", || complete(&w, &a));
    for body in bodies(&w, &format!("/f/{FLEET}/pub/{}/>", w.node)) {
        if !body.starts_with("v=2") {
            continue;
        }
        for leak in [
            "SECRET", " text=", " title=", " cwd=", " from=", " to=", " off=", " cause=", " re=",
            " inbox=", " dir=",
        ] {
            assert!(!body.contains(leak), "{leak} crossed: {body}");
        }
    }
}

/// `aterm fabric brief` folds what the bus holds, with the GUI's own `brief`
/// saying the same need.
#[test]
fn fabric_brief_folds_what_the_bus_holds() {
    let w = turns("lm-brief", &[]);
    let (a, _) = w.two_sessions();
    mark(&w, &a, "need");
    until("the mirror to be complete", || complete(&w, &a));
    let local = w.verb(&format!("@{a} brief"));
    assert!(local.ok(), "{}", local.header());
    let local_text = format!("{}\n{}", local.header(), local.rows().join("\n"));
    assert!(
        local_text.contains("claim need review by=ctl"),
        "{local_text}"
    );
    let fabric = std::fs::read_to_string(w.tmp.join("fabric.cmd")).expect("the command");
    let mut cmd = aterm_uds::spawnfd::command(super::harness::aterm_to_launch());
    cmd.args(["link", "fabric", "brief", &format!("@{a}")])
        .env("ATERM_FABRIC_COMMAND", &fabric)
        .env("HOME", w.tmp.join("home"))
        .env("XDG_CONFIG_HOME", w.tmp.join("cfg"))
        .env("XDG_RUNTIME_DIR", w.tmp.join("run"))
        .stdin(aterm_uds::spawnfd::Stdio::null());
    let out = aterm_uds::deadline::output_until_complete_stdout(
        &mut cmd,
        std::time::Instant::now() + super::harness::DEADLINE,
    )
    .expect("aterm fabric brief exits with complete stdout before the hang detector");
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "{text}{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(text.starts_with("OK "), "{text}");
    assert!(text.contains(&format!("@{a}@{}", w.node)), "{text}");
    assert!(
        text.contains("claim need review by=ctl"),
        "the bus fold says the same need: {text}"
    );
}
