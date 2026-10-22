// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE BUS MIRROR END TO END (`docs/DESIGN-session-log-2026-10-15.md` §5.5):
//! a real headless aterm, its real bridge and a guarded
//! broker. With `--log off` it writes nothing anywhere; by default (`auto`,
//! over the world's broker, which retains by aterm's default rules)
//! it mirrors as `--log turns` does, and over a broker
//! that retains nothing it mirrors nothing and says `mirror=unretained`. With
//! the mirror on every session's bus-class records reach its `ev` face, each `(ep, seq)`
//! once in the reader's view, `ms` dense, with one exact cover
//! per hole an eviction made, no host or content text crossing,
//! and `aterm fabric brief` folding what the bus holds. Every wait is a
//! one-minute hang detector.
//!
//! crashes — a bridge killed at each step of a batch, a broker
//! restart (or a power loss that moves the log) with a pin outstanding — are
//! `aterm-link`'s deterministic simulation's (`sim`): it kills the real bridge
//! at every I/O step of every seed with the mirror on and holds the reader's
//! view to the same laws, no raw duplicate where the log never moved among
//! them. This world keeps the real instance's side: the GUI's take and
//! landing, astream's own dedup, a real process tree.

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
/// a rate, never a law).
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

/// Sessions write claims and one exits; then, past a drain barrier through
/// the same bridge, whether the bus holds no `v=2` body on any face and the
/// state dir no `logpin` and none of its durable copies (`logkey`,
/// `logfloor`): what a bridge that does not mirror leaves.
fn mirrors_nothing(w: &World) {
    let (a, b) = w.two_sessions();
    claims(w, &a, 3);
    claims(w, &b, 3);
    assert!(w.verb(&format!("@{b} close")).ok());
    // The bridge's own `ev` records still flow; give it a round to publish
    // whatever it would: a drain barrier through the same bridge.
    w.drained(&a);
    let all = bodies(w, &format!("/f/{FLEET}/pub/>"));
    assert!(
        all.iter().all(|b| aterm_slog::decode_bus(b).is_err()),
        "a v=2 body on the bus with the mirror off: {:?}",
        all.iter().find(|b| b.starts_with("v=2"))
    );
    for file in ["logpin", "logkey", "logfloor"] {
        assert!(!w.state.join(file).exists(), "no {file} is ever written");
    }
}

/// a bridge with `--log off` (`[fabric] log = "off"`,
/// the opt-out) mirrors nothing: no
/// `v=2` body on any face, no `logpin` and none of its durable copies, and
/// `fabric status` says nothing of a mirror. NEGATIVE CONTROL, run: `off`
/// read as `auto` fails it.
#[test]
fn log_off_mirrors_nothing_and_writes_nothing() {
    let w = World::boot_flags_with("lm-off", &["--log", "off"], &[]);
    w.wait_ready();
    mirrors_nothing(&w);
    let fs = w.verb("fabric status");
    assert!(!fs.header().contains("bridge_log="), "{}", fs.header());
}

/// THE DEFAULT MIRRORS TURNS (owner decision D4):
/// with no `--log` and no `[fabric] log`, over the world's broker,
/// which retains by aterm's default rules and advertises them in its
/// `Hello`, every live turn-class record of a session reaches its `ev` face,
/// once, `ms` dense, and `fabric status` says the mirror is on. NEGATIVE
/// CONTROL, run: an attach that does not follow the broker (no
/// `mirror_attached`) fails it.
#[test]
fn the_default_mirrors_turns() {
    let w = World::boot("lm-default", &[]);
    w.wait_ready();
    let (a, _b) = w.two_sessions();
    claims(&w, &a, 4);
    let m = until("the session's mirror to be quiescent and complete", || {
        complete(&w, &a)
    });
    assert!(
        m.raw.values().all(|n| *n == 1),
        "a duplicate on the bus: {:?}",
        m.raw
    );
    let fs = w.verb("fabric status");
    assert!(
        fs.header().contains(" bridge_log=turns mirror=on"),
        "{}",
        fs.header()
    );
}

/// THE DEFAULT FOLLOWS THE BROKER: over
/// `World::without_retention`'s broker, which retains nothing and advertises
/// no rule, the default bridge mirrors nothing — no `v=2` body, no `logpin`,
/// `logkey` or `logfloor` — and `fabric status` says `bridge_log=auto
/// mirror=unretained`. NEGATIVE CONTROL, run: an `auto` mirror that does not
/// ask the broker's `Hello` (the gate always open) fails it.
#[test]
fn the_default_over_an_unretaining_broker_mirrors_nothing_and_says_so() {
    let w = World::without_retention("lm-unret");
    w.wait_ready();
    mirrors_nothing(&w);
    let fs = w.verb("fabric status");
    assert!(
        fs.header().contains(" bridge_log=auto mirror=unretained"),
        "{}",
        fs.header()
    );
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
    // the pin's key and total have their
    // durable copies, written by the real bridge on the real filesystem — the
    // key in `logkey` (owner-only, one line), and a floor at or above the
    // total in `logfloor` — so a power cut that tears the pin stops nothing.
    let pin = until("a pin between batches", || {
        let text = std::fs::read_to_string(w.state.join("logpin")).ok()?;
        aterm_link::mirror::Pin::parse(&text)
            .ok()
            .filter(|p| p.slots.is_empty() && p.total > 0)
    });
    let key = std::fs::read_to_string(w.state.join("logkey")).expect("logkey");
    assert_eq!(
        Some(key.trim_end().to_string()),
        pin.key
            .map(|k| k.iter().map(|b| format!("{b:02x}")).collect::<String>()),
        "the pin's key is logkey's"
    );
    assert_eq!(key.len(), 33, "32 hex digits and a newline: {key:?}");
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(w.state.join("logkey"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0, "the key is its owner's alone: {mode:o}");
    }
    let floor: u64 = std::fs::read_to_string(w.state.join("logfloor"))
        .expect("logfloor")
        .trim_end()
        .parse()
        .expect("one number");
    assert!(
        floor >= pin.total,
        "the floor {floor} covers the total {}",
        pin.total
    );
    // The second session exits: its close reaches the bus too.
    assert!(w.verb(&format!("@{b} close")).ok());
    until("the closed session's close on the bus", || {
        let m = mirrored(&w, &b);
        let (_, space) = m.spaces.iter().next_back()?;
        let (fold, _) = space.fold();
        fold.state().closed.is_some().then_some(())
    });
    // once its close has landed its `ev` face is retired — out of
    // the last-value index, its subject back in the node's budget — and its
    // records stay in the log, fetchable.
    let face = format!("/f/{FLEET}/pub/{}/{b}/ev", w.node);
    until("the closed session's ev face to be retired", || {
        let rows = w.read(|c| c.last(&face, "", 8)).ok()?.0;
        rows.iter().all(|(_, s, _)| *s != face).then_some(())
    });
    assert!(mirrored(&w, &b).v2 > 0, "the records stay");
}

/// with the broker away, a small ring evicts the claims the mark
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
