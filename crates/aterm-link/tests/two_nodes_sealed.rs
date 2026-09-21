// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
#![cfg(feature = "sealed")]
//! Compiled only with `--features sealed`: this rung is the one place aterm
//! builds astream's cipher tree, and a default build (and every shipped binary)
//! does not. `targo --unverified test -p aterm-link --features sealed`.

//! **A5 — two nodes, sealed, cross-host.**
//!
//! Everything A3 proved on one node, proved again with the two peers in
//! DIFFERENT `aterm-gui` processes with different node identities, talking
//! through one broker over `--tcp --key-file`: astream's XChaCha20-Poly1305
//! sealed record layer under a pre-shared key (§8.6). Nothing here is a mock —
//! the endpoints are shipped `aterm-gui`s, the bridges are shipped
//! `aterm-link serve` children they launched over inherited socketpairs, the
//! broker enforces capabilities on every attach, and the wire is sealed: a
//! process without the key cannot complete the handshake, let alone a verb.
//!
//! ## Why loopback is not a weakened claim
//!
//! The sealed transport is a property of the CONNECTION, not of the route: the
//! same two hellos, the same per-record AAD binding direction and sequence, and
//! the same key-confirming record each way run whether the ends are one host or
//! two. What a second machine would add is a network that reorders, drops and
//! delays — astream's own problem, and its own tests. What this file adds is
//! everything that needs two NODES, which is what A3 could not have.
//!
//! ## No sleeps as synchronisation
//!
//! Every wait is `until <observable state>`: a record on the log, a field in a
//! reply, a process's own output. The bound is a HANG DETECTOR.

#![cfg(unix)]

mod harness;

use std::time::Duration;

use aterm_link::transport::Conn;
use harness::{until, until_within, Fleet, DEADLINE, FLEET, PERIODIC_DEADLINE};

/// Wait for a last-value row to carry `what`, ON ONE REUSED CONNECTION.
///
/// The connection is the point. `Fleet::god()` is a fresh sealed TCP connect, a
/// handshake, an HMAC mint and a cap attach; opening one INSIDE a poll closure
/// runs all four every [`harness::POLL_GAP`] — up to six thousand broker
/// connections for a single wait that is only slow because the machine is busy,
/// which is the load that made this file's own waits time out. One connection
/// per wait is the same observation, without the harness competing with the
/// bridge for the broker it is waiting on.
///
/// `budget` is stated by the caller, because the two classes of wait here are
/// not the same: most rows are published by the arrival that caused them, and a
/// few by the bridge's periodic backstop (see [`harness::until_within`]).
fn row_says(watch: &mut Conn, subject: &str, what: &str, budget: Duration) {
    until_within(budget, &format!("{subject} to say {what}"), || {
        let (rows, _) = watch.last(subject, "", 8).ok()?;
        rows.into_iter()
            .find(|(_, s, _)| s == subject)
            .filter(|(_, _, x)| String::from_utf8_lossy(x).contains(what))
            .map(|_| ())
    });
}

/// **The whole A3 flow, across two nodes, on the sealed wire — and `ls` sees
/// both.**
///
/// A's session posts to B's session by its bare `@s-<sid>`; A's bridge has
/// never heard of that sid, so it resolves it the way §6.1 says — from the
/// roster, pinned on first sight — publishes under its own bound cap, and B's
/// bridge picks it off its own durable group and delivers it. B answers `re=`
/// the offset A was told, and A's inbox resolves that back to A's own post id.
///
/// The offsets are the broker's, so the correlation id nobody can forge is the
/// same one across two hosts as it was across two sessions.
#[test]
fn a_post_crosses_two_nodes_over_the_sealed_wire_and_ls_sees_both() {
    let mut fleet = Fleet::boot("cross");
    let a = fleet.node("cross-a", &[]);
    let b = fleet.node("cross-b", &[]);
    a.wait_ready(&fleet);
    b.wait_ready(&fleet);
    let (sid_a, _) = a.session();
    let (sid_b, _) = b.session();
    assert_ne!(a.node, b.node, "two nodes, two identities");

    // `ls` FIRST, before any traffic: the roster is presence, not history.
    // Both nodes' sessions must be listed by ONE `Last` round trip from a host
    // that hosts nothing — that is the whole claim of the cross-host `ls` (§7).
    // §7'S ROW IS `<node> <host> <sid> …`, so the node and the session are the
    // FIRST and THIRD columns with the host between them — a session row has no
    // `host=` of its own and prints `-` there (`main.rs`'s module header says
    // which columns have no writer yet and why).
    let names = |out: &str, node: &str, sid: &str| {
        out.lines().any(|l| {
            let c: Vec<&str> = l.split_whitespace().collect();
            c.first() == Some(&node) && c.get(2) == Some(&sid)
        })
    };
    let listing = until("aterm-link ls to list both nodes' sessions", || {
        let out = fleet.ls();
        (names(&out, &a.node, &sid_a) && names(&out, &b.node, &sid_b)).then_some(out)
    });
    for node in [&a.node, &b.node] {
        assert!(
            names(&listing, node, "node") && listing.contains("state=live"),
            "ls lost a node's own presence row:\n{listing}"
        );
    }
    // AND §7'S COLUMN SET IS THERE, not a set of `ls`'s own. §9.3 sells
    // `attention=` as the escalation view and A8's glance guarantees it; a
    // roster that quietly dropped the column would leave both with no face.
    for column in [
        "state=",
        "inc=",
        "role=",
        "detail=",
        "driving=",
        "holder=",
        "hold=",
        "fabric=",
        "attention=",
        "gen=",
    ] {
        assert!(listing.contains(column), "ls dropped {column}:\n{listing}");
    }
    // A SESSION ROW CARRIES ITS LIVE GENERATION. `gen=` is a KNOWN body field,
    // so a reader that pulled it out of the unknown-token map would print `-`
    // for every session and nobody would notice until a `gen=`-bound approval
    // was minted against a dash.
    for sid in [&sid_a, &sid_b] {
        let row = listing
            .lines()
            .find(|l| l.contains(sid.as_str()))
            .unwrap_or_else(|| panic!("ls lost {sid}:\n{listing}"));
        let gen = row
            .split_whitespace()
            .find_map(|t| t.strip_prefix("gen="))
            .unwrap_or("-");
        assert!(
            gen.contains(':') && gen != "-",
            "a session row must carry `<content_seq>:<fp16>`: {row}"
        );
    }

    // THE POST. `--wait` answers the broker offset the record landed at.
    let reply = a.verb(&format!(
        "@{sid_a} post to=@{sid_b} kind=ask --wait A5 hello"
    ));
    assert!(
        reply.ok(),
        "post across the sealed wire: {} / {}",
        reply.header(),
        a.log_tail()
    );
    let off: u64 = reply
        .header()
        .split_whitespace()
        .find_map(|t| t.strip_prefix("off="))
        .expect("a landed post answers off=")
        .parse()
        .expect("an offset is a number");

    let row = until("the ask to land in B's inbox", || {
        b.inbox(&sid_b).into_iter().find(|r| r.contains("kind=ask"))
    });
    assert!(
        row.contains(&format!("from={sid_a}@{}", a.node)),
        "B must see A's SESSION, attested by A's NODE: {row}"
    );
    assert!(row.contains("trust=agent"), "{row}");
    assert!(row.contains(&format!("off={off}")), "{row}");

    // THE ANSWER, back the other way, correlated by the offset A was told.
    let id: u64 = row
        .split_whitespace()
        .nth(1)
        .and_then(|t| t.parse().ok())
        .expect("an inbox row is `msg <id> …`");
    let answer = b.verb(&format!(
        "@{sid_b} post to=@{sid_a}@{} kind=answer re={off} --wait A5 pong",
        a.node
    ));
    assert!(answer.ok(), "answer: {}", answer.header());
    let back = until("A to see the answer resolved to its own post id", || {
        a.inbox(&sid_a)
            .into_iter()
            .find(|r| r.contains("kind=answer"))
    });
    assert!(back.contains(&format!("re={off}")), "{back}");
    assert!(
        back.contains("re-id=1"),
        "the answer must resolve to A's OWN post id: {back}"
    );
    assert!(id > 0);
}

/// **Two nodes advertising ONE sid: `post` answers `ERR ambiguous` and delivers
/// nothing.**
///
/// A3 built the TOFU pin — first-wins, survives a reopen — but could not test
/// the CONFLICT, because a conflict needs a second claimant. §6.1 states the
/// threat exactly: "a node's cap covers its whole `pub/<n>/` subtree, so a rogue
/// node *can* publish a presence row for a sid it does not host; pinning stops
/// that from becoming a route into the rogue's own read lane."
///
/// HOW THE ROGUE IS BUILT, said plainly. Sids are minted by the instance that
/// hosts them, so no second aterm can ever host B's sid and the conflict cannot
/// be staged by running a third bridge honestly. It is staged the way the design
/// says it arises: a third node id with its own minted cap publishes a presence
/// row for B's session under its own `pub/<n>/` subtree. That is a real
/// capability, a real record, and a real second advertiser — the only thing it
/// is not is a process that would ever do it by accident.
///
/// The order is the point. A's bridge pins `sid → B` on the first post, and it
/// is the pinned sid a second claimant then contests: the pin is what turns "who
/// gets the traffic" into "nobody, and say so".
#[test]
fn two_nodes_advertising_one_sid_make_post_answer_err_ambiguous() {
    let mut fleet = Fleet::boot("ambig");
    let a = fleet.node("ambig-a", &[]);
    let b = fleet.node("ambig-b", &[]);
    a.wait_ready(&fleet);
    b.wait_ready(&fleet);
    let (sid_a, _) = a.session();
    let (sid_b, _) = b.session();

    // Node readiness precedes publication of its session roster. Bare-SID
    // routing needs B's session advertisement, so wait for that observable
    // before submitting a post that must not be retried after it lands.
    let presence = format!("/f/{FLEET}/pub/{}/{sid_b}/presence", b.node);
    let mut watch = fleet.god();
    row_says(&mut watch, &presence, "state=live", DEADLINE);

    // ONE GOOD POST FIRST — it pins `sid_b → B`.
    let first = a.verb(&format!(
        "@{sid_a} post to=@{sid_b} kind=note --wait pinning"
    ));
    assert!(first.ok(), "the pinning post: {}", first.header());
    until("the pin to reach A's state dir", || {
        let pins = std::fs::read_to_string(a.state.join("pins")).ok()?;
        pins.contains(&format!("{sid_b} {}", b.node)).then_some(())
    });
    // SYNCHRONISE ON THE DELIVERY, not on the publish. `post --wait` answers as
    // soon as the record is on the LOG; B's bridge delivering it is a separate
    // hop, and sampling the count before that hop finished would compare a
    // "before" of 0 against an "after" of 1 that the CONTESTED post had nothing
    // to do with.
    let delivered_before = until("the pinning post to land at B", || {
        let rows = b.inbox(&sid_b);
        (!rows.is_empty()).then_some(rows.len())
    });

    // THE ROGUE. A node id nobody runs a bridge for, holding a real minted cap
    // over its own subtree, publishing a presence row for a session it does not
    // host.
    let rogue = "n-00000000deadbeef".to_string();
    let cap_path = fleet.cap_file("rogue", &[format!("rw,p={rogue}:/f/{FLEET}/pub/{rogue}/>")]);
    let caps = aterm_link::bridge::read_cap_file(cap_path.to_string_lossy().as_ref())
        .expect("read the rogue cap");
    let mut client = astream_broker::Client::connect_tcp_sealed(&fleet.addr, fleet.key())
        .expect("the rogue reaches the broker over the same sealed wire");
    for cap in &caps {
        client
            .attach(&cap.grant, &cap.tag)
            .expect("the rogue's cap attaches — it is a real capability");
    }
    let subject = format!("/f/{FLEET}/pub/{rogue}/{sid_b}/presence");
    let body = "v=1 t=1 inc=1 epoch=deadbeef state=live hold=0 fabric=connected";
    client
        .publish(
            astream_cap::producer_id_of(&rogue),
            1,
            &subject,
            body.as_bytes(),
        )
        .expect("the rogue publishes its claim");

    // NOW THE SECOND POST. The bridge reads the roster at resolve time, sees two
    // claimants for a sid it had pinned, and refuses.
    let second = a.verb(&format!(
        "@{sid_a} post to=@{sid_b} kind=note --wait contested"
    ));
    assert!(
        second.header().starts_with("ERR ambiguous"),
        "a contested sid must answer `ERR ambiguous`, got {:?}\n{}",
        second.header(),
        a.log_tail()
    );
    // The `ev` record is a SECOND, independent hop: `post --wait` wakes on the
    // endpoint's retirement, and the digest record is published after it. Two
    // separate reads of the digest — one for the condition, one for the failure
    // message — would race each other, so this waits on the observation.
    until("the refusal to reach the node's own digest", || {
        a.ev(&fleet)
            .iter()
            .any(|e| e.contains("undeliverable") && e.contains("reason=ambiguous"))
            .then_some(())
    });

    // AND NOTHING WAS DELIVERED. Not to the rogue's lane, and not to B's: the
    // whole point of refusing is that the message did not go to the wrong place.
    assert_eq!(
        b.inbox(&sid_b).len(),
        delivered_before,
        "an ambiguous post must deliver NOTHING"
    );
    let mut god = fleet.god();
    let rogue_lane = format!("/f/{FLEET}/in/{rogue}/>");
    let (rows, _) = god
        .fetch(0, &rogue_lane, 64)
        .expect("read the rogue's lane");
    assert!(
        rows.is_empty(),
        "a message reached the rogue's read lane: {rows:?}"
    );
}

/// **Observer mode delivers nothing, and never makes a session ambiguous.**
///
/// A hand-started `aterm-link serve --sock … --token-file …` holds an ordinary
/// Owner token, so it is not the bridge: §11.2 gives it "presence and `ev` only,
/// no delivery, no hold". A3 implemented that and never ran it; this is the run.
///
/// The second half is the interaction A3 could not have seen, because it needs
/// two nodes. An observer publishes presence rows for sessions it WATCHES and
/// does not host. §6.1 routes a bare `@s-<sid>` only when exactly one node
/// advertises it. Unmarked, opening a read-only observer would therefore make
/// every session it could see ambiguous fleet-wide — a working fabric broken by
/// a tool that writes nothing. The rows carry `observer=1` and the resolver
/// skips them, so this test posts across the fleet WITH an observer running.
#[test]
fn an_observer_delivers_nothing_and_never_makes_a_session_ambiguous() {
    let mut fleet = Fleet::boot("obs");
    let a = fleet.node("obs-a", &[]);
    let b = fleet.node("obs-b", &[]);
    a.wait_ready(&fleet);
    b.wait_ready(&fleet);
    let (sid_a, _) = a.session();
    let (sid_b, _) = b.session();

    // The observer is its own node: its own id, its own caps, its own state dir.
    let watcher = "n-0000000000005ee5".to_string();
    let state = fleet.tmp.join("watch");
    std::fs::create_dir_all(&state).expect("observer state dir");
    std::fs::write(state.join("node"), format!("{watcher}\n")).expect("seed the observer id");
    let cap_path = fleet.cap_file("watch", &harness::node_grants(&watcher));
    let token_file = fleet.tmp.join("watch.token");
    std::fs::write(&token_file, format!("{}\n", b.token)).expect("write the token file");
    let log = fleet.tmp.join("watch.log");
    let out = std::fs::File::create(&log).expect("observer log");
    let err = out.try_clone().expect("observer log clone");
    let child = std::process::Command::new(env!("CARGO_BIN_EXE_aterm-link"))
        .args([
            "serve",
            "--fleet",
            FLEET,
            "--broker",
            &fleet.addr,
            "--tcp",
            "--key-file",
            fleet.key_file.to_string_lossy().as_ref(),
            "--cap-file",
            cap_path.to_string_lossy().as_ref(),
            "--state",
            state.to_string_lossy().as_ref(),
            "--sock",
            &b.ctl_sock,
            "--token-file",
            token_file.to_string_lossy().as_ref(),
        ])
        .stdin(std::process::Stdio::null())
        .stdout(out)
        .stderr(err)
        .spawn()
        .expect("hand-start an observer");
    // Killed on every exit path, panics included: nothing here is supervised,
    // so a leaked observer would outlive the test and keep publishing.
    struct Reaper(std::process::Child);
    impl Drop for Reaper {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let _reaper = Reaper(child);

    // IT SAYS SO. A bridge that discovered its own powerlessness one refusal at
    // a time would be a bridge nobody could diagnose.
    until("the observer to announce itself", || {
        std::fs::read_to_string(&log)
            .ok()?
            .contains("OBSERVER MODE")
            .then_some(())
    });
    // It publishes presence — and marks the sessions it does not host.
    let mut watch = fleet.god();
    let observer_row = format!("/f/{FLEET}/pub/{watcher}/{sid_b}/presence");
    row_says(&mut watch, &observer_row, "observer=1", DEADLINE);
    let listing = fleet.ls();
    assert!(
        listing.lines().any(|l| {
            let c: Vec<&str> = l.split_whitespace().collect();
            c.first() == Some(&watcher.as_str())
                && c.get(2) == Some(&sid_b.as_str())
                && l.contains("observer=1")
        }),
        "ls must show the observer for what it is:\n{listing}"
    );

    // IT DELIVERS NOTHING. A message on the observer's OWN lane is recorded
    // undeliverable rather than pushed into a session it has no authority over.
    let mut god = fleet.god();
    let lane = format!("/f/{FLEET}/in/{watcher}/{sid_b}/h-andrew/note");
    god.publish(7_300, 1, &lane, b"v=1 t=1 text=hello")
        .expect("publish to the observer's lane");
    let ev_face = format!("/f/{FLEET}/pub/{watcher}/node/ev");
    until("the observer to record it undeliverable", || {
        let (rows, _) = watch.fetch(0, &ev_face, 256).ok()?;
        rows.iter()
            .any(|(_, _, x)| String::from_utf8_lossy(x).contains("observer"))
            .then_some(())
    });
    assert!(
        b.inbox(&sid_b).is_empty(),
        "an observer delivered a row: {:?}",
        b.inbox(&sid_b)
    );

    // AND THE FLEET STILL ROUTES. The observer advertises B's session under its
    // own node id; if that counted, this post would be `ERR ambiguous`.
    let reply = a.verb(&format!(
        "@{sid_a} post to=@{sid_b} kind=note --wait through"
    ));
    assert!(
        reply.ok(),
        "an observer broke routing: {} / {}",
        reply.header(),
        a.log_tail()
    );
    until("the message to arrive at B despite the observer", || {
        (!b.inbox(&sid_b).is_empty()).then_some(())
    });
}
