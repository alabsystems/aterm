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

/// **A human's claim, then a generation-bound keystroke: applied ONCE, refused
/// when the generation moved, refused again after a relaunch to a new epoch.**
///
/// §6.6's one rule, end to end and across a real process boundary. The
/// keystroke is applied only when `<src>` holds the keyboard AT APPLY TIME, the
/// session is not held, the body's `epoch=` is the live launch nonce, and its
/// `gen=` is the live `<content_seq>:<fp16>`. The replay is byte-identical
/// except for its producer sequence, so what refuses it is the fence and
/// nothing else — and `gen=` is the fence that catches the case `epoch=` cannot:
/// the same session, still live, with a screen that has moved on since the human
/// read it.
#[test]
fn a_claim_and_a_generation_bound_keystroke_apply_once() {
    let mut fleet = Fleet::boot("gen");
    // The human must be on the allowlist or its `control` is demoted to a note
    // (§8.4) and never reaches the handoff at all.
    let mut b = fleet.node("gen-b", &["h-andrew"]);
    b.wait_ready(&fleet);
    let (sid, epoch) = b.session();
    let mut god = fleet.god();
    let mut seq = 0u64;
    let mut publish = |subject: &str, body: &[u8]| {
        seq += 1;
        god.publish(7_100, seq, subject, body)
            .expect("publish as the human")
    };

    // 1 — THE CLAIM.
    let claim_lane = format!("/f/{FLEET}/in/{}/{sid}/h-andrew/control", b.node);
    let claim = format!("v=1 t=1 epoch={epoch} text=claim");
    publish(&claim_lane, claim.as_bytes());
    let control_row = format!("/f/{FLEET}/pub/{}/{sid}/control", b.node);
    let mut watch = fleet.god();
    row_says(&mut watch, &control_row, "holder=h-andrew", DEADLINE);
    // THE MIRROR LEASE IS TAKEN, and it is what a local driver sees. §6.6 row 5
    // is published by the bridge's PERIODIC backstop, not by the arrival that
    // caused it, so it is budgeted as a periodic wait.
    until_within(
        PERIODIC_DEADLINE,
        "aterm's own lease to mirror the fabric holder",
        || {
            b.verb(&format!("@{sid} lease status"))
                .header()
                .contains("holder=fabric:h-andrew")
                .then_some(())
        },
    );

    // 2 — THE KEYSTROKE, bound to the screen the human read. No newline: the
    // shell ECHOES it onto the command line and runs nothing, so the mark
    // appears exactly once per application and a second application would be
    // visible as a second copy.
    const MARK: &str = "A5XDRIVE";
    let drive_lane = format!("/f/{FLEET}/term/{}/{sid}/in/h-andrew", b.node);
    let gen = b.gen(&sid);
    let keystroke = format!("v=1 t=1 epoch={epoch} gen={gen} len={}\n{MARK}", MARK.len());
    publish(&drive_lane, keystroke.as_bytes());
    until("the keystroke to reach the PTY", || {
        b.verb(&format!("@{sid} text"))
            .rows()
            .join("\n")
            .contains(MARK)
            .then_some(())
    });

    // 3 — THE REPLAY, with the SAME `gen=`, after the screen has moved. The
    // keystroke itself is what moved it, which is the honest shape of the
    // hazard: the human answered a prompt that is no longer on the screen.
    until("the generation to move", || {
        (b.gen(&sid) != gen).then_some(())
    });
    publish(&drive_lane, keystroke.as_bytes());
    until("the stale generation to be refused and recorded", || {
        b.ev(&fleet)
            .iter()
            .any(|e| e == &format!("refused sid={sid} face=term reason=gen"))
            .then_some(())
    });
    let screen = b.verb(&format!("@{sid} text")).rows().join("\n");
    assert_eq!(
        screen.matches(MARK).count(),
        1,
        "the keystroke was applied more than once:\n{screen}"
    );

    // 4 — A RELAUNCH TO A NEW EPOCH. aterm mints a fresh sid and a fresh launch
    // nonce at every launch, so the successor session is a different session
    // with a different epoch — and the claim the human minted against the old
    // one must not land on it. Nor may the keystroke that rode on that claim.
    b.relaunch();
    b.wait_ready(&fleet);
    let (sid2, epoch2) = b.session();
    assert_ne!(sid2, sid);
    assert_ne!(epoch2, epoch);
    let stale_claim = format!("/f/{FLEET}/in/{}/{sid2}/h-andrew/control", b.node);
    publish(&stale_claim, claim.as_bytes());
    until("the stale claim to be refused on the new session", || {
        b.ev(&fleet)
            .iter()
            .any(|e| e == &format!("refused sid={sid2} face=control reason=epoch"))
            .then_some(())
    });
    let stale_drive = format!("/f/{FLEET}/term/{}/{sid2}/in/h-andrew", b.node);
    publish(&stale_drive, keystroke.as_bytes());
    until("the stale keystroke to be refused too", || {
        b.ev(&fleet)
            .iter()
            .any(|e| e.starts_with(&format!("refused sid={sid2} face=term")))
            .then_some(())
    });
    let screen = b.verb(&format!("@{sid2} text")).rows().join("\n");
    assert!(
        !screen.contains(MARK),
        "a claim and a keystroke from a previous epoch typed into its successor:\n{screen}"
    );
    // The row on the bus still names the OLD session's holder and the new one
    // has none — the refusal moved nothing.
    let (rows, _) = fleet
        .god()
        .last(&format!("/f/{FLEET}/pub/{}/{sid2}/control", b.node), "", 8)
        .expect("read the new session's control row");
    assert!(
        rows.is_empty(),
        "the refused claim still published a control row: {rows:?}"
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

/// **The rest of §6.6's table, end to end — and the mirror lease is RENEWED.**
///
/// A3 built one row of the handoff table (an `h-*` claim) and issued its mirror
/// `lease acquire holder=fabric:<p> ttl=30000` exactly once. §6.6 says the lease
/// is "renewed while the row stands", and the TTL is 30 s, so the mirror lapsed
/// half a minute into every handoff: the bus row still said `holder=h-andrew`
/// while aterm's `who` had stopped saying `driving=lease:fabric:h-andrew` and a
/// competing local `turn` had stopped answering `ERR busy`. This test watches
/// the expiry go back UP, which is the only observation that distinguishes a
/// renewed lease from one that merely has not lapsed yet.
///
/// It costs about ten seconds of wall clock, and that is not a performance
/// assertion: the renewal period is a third of the TTL, so a shorter test would
/// be a test of nothing. The `until` bound around it is still a hang detector.
#[test]
fn the_handoff_table_moves_the_keyboard_and_the_mirror_lease_is_renewed() {
    let mut fleet = Fleet::boot("handoff");
    let b = fleet.node("handoff-b", &["h-andrew", "a-worker"]);
    b.wait_ready(&fleet);
    let (sid, epoch) = b.session();
    let mut god = fleet.god();
    let mut seq = 0u64;
    let control_row = format!("/f/{FLEET}/pub/{}/{sid}/control", b.node);
    let mut send = |who: &str, text: &str| {
        seq += 1;
        let lane = format!("/f/{FLEET}/in/{}/{sid}/{who}/control", b.node);
        // `text=` is ONE whitespace-delimited token on the body line, so a
        // two-word op (`grant <p>`) is pct-encoded exactly as every other body
        // encodes its text (§4.1).
        let body = format!("v=1 t=1 epoch={epoch} text={}", text.replace(' ', "%20"));
        god.publish(7_500, seq, &lane, body.as_bytes())
            .expect("publish a control message");
    };
    // ONE READER for the whole test (see [`row_says`]): this test alone made up
    // to six thousand sealed connections per wait, which is load the harness
    // added to the machine it was timing.
    let mut watch = fleet.god();

    // ROW 1 — the human claims, unconditionally.
    send("h-andrew", "claim");
    row_says(&mut watch, &control_row, "holder=h-andrew", DEADLINE);

    // ROW 2 — an agent asks while a human holds it: PENDING, not granted. This
    // is the row that would otherwise let a prompt-injected worker take the
    // wheel out of a person's hands by asking politely.
    send("a-worker", "request");
    until("the agent's request to be recorded as pending", || {
        let (rows, _) = watch.last(&control_row, "", 8).ok()?;
        rows.into_iter()
            .find(|(_, s, _)| *s == control_row)
            .filter(|(_, _, x)| {
                let body = String::from_utf8_lossy(x);
                body.contains("pending=a-worker") && body.contains("holder=h-andrew")
            })
            .map(|_| ())
    });

    // ROW 3 — the holder releases; the agent's next request now lands.
    send("h-andrew", "release");
    row_says(&mut watch, &control_row, "holder=-", DEADLINE);
    // §6.6 ROW 5 AGAIN: the local lease mirror is periodic, not event-driven.
    until_within(
        PERIODIC_DEADLINE,
        "aterm's own lease to be dropped with it",
        || {
            b.verb(&format!("@{sid} lease status"))
                .header()
                .contains("lease none")
                .then_some(())
        },
    );
    send("a-worker", "request");
    row_says(&mut watch, &control_row, "holder=a-worker", DEADLINE);

    // ROW 3b — a human may grant it to somebody else even while another holds.
    send("h-andrew", "grant h-partner");
    row_says(&mut watch, &control_row, "holder=h-partner", DEADLINE);

    // A THIRD PARTY MAY NOT. The refusal is recorded and the row does not move.
    send("a-worker", "grant a-worker");
    until("the forged grant to be refused", || {
        b.ev(&fleet)
            .iter()
            .any(|e| e == &format!("refused sid={sid} face=control reason=holder"))
            .then_some(())
    });
    let (rows, _) = watch
        .last(&control_row, "", 8)
        .expect("read the control row");
    assert!(
        String::from_utf8_lossy(&rows[0].2).contains("holder=h-partner"),
        "a refused grant moved the row: {:?}",
        String::from_utf8_lossy(&rows[0].2)
    );

    // THE RENEWAL. The expiry decays with the clock and jumps back to the full
    // TTL when the bridge renews. Nothing but a renewal can make it INCREASE.
    let expiry = || -> Option<u64> {
        b.verb(&format!("@{sid} lease status"))
            .header()
            .split_whitespace()
            .find_map(|t| t.strip_prefix("expires_in_ms=")?.parse().ok())
    };
    until_within(
        PERIODIC_DEADLINE,
        "the mirror lease to exist at all",
        || {
            b.verb(&format!("@{sid} lease status"))
                .header()
                .contains("holder=fabric:h-partner")
                .then_some(())
        },
    );
    let mut lowest = u64::MAX;
    until_within(
        PERIODIC_DEADLINE,
        "the mirror lease's expiry to go back UP — a renewal",
        || {
            let header = b.verb(&format!("@{sid} lease status")).header().to_string();
            assert!(
                header.contains("holder=fabric:h-partner"),
                "the mirror lease went away while we watched it: {header}"
            );
            let now = expiry()?;
            let renewed = now > lowest.saturating_add(1_000);
            lowest = lowest.min(now);
            renewed.then_some(())
        },
    );

    // ROW 4 — THE CONSERVATIVE PAUSE. Somebody drives the session from a LOCAL
    // socket: the bridge did not cause it, cannot attribute it, and parks the
    // row at `human?` until a principal claims it again. `human?` is not a
    // principal by §3.2's grammar, so no `<src>` segment can ever equal it and
    // the session applies NOTHING from the bus while it is parked — the refusal
    // reads as an explanation instead of a silence.
    b.verb(&format!("@{sid} send sleep 2"));
    b.verb(&format!("@{sid} key enter"));
    // THE CONSERVATIVE PAUSE IS DISCOVERED BY LOOKING. `sample_local_control` is
    // its only producer and it runs on the bridge's periodic backstop, so this
    // wait is a wait on the machine PLUS up to a full round — budgeted as such,
    // not as the event-driven waits above. A 60 s ceiling here was the harness
    // timing a duty cycle and reporting a healthy bridge as broken.
    row_says(&mut watch, &control_row, "holder=human?", PERIODIC_DEADLINE);
    row_says(
        &mut watch,
        &control_row,
        "evidence=unaccounted-change",
        PERIODIC_DEADLINE,
    );
    let drive = format!("/f/{FLEET}/term/{}/{sid}/in/h-partner", b.node);
    let live_gen = b.gen(&sid);
    let body = format!("v=1 t=1 epoch={epoch} gen={live_gen} len=6\nPAUSED");
    watch
        .publish(7_600, 1, &drive, body.as_bytes())
        .expect("publish a keystroke into a paused session");
    until("the paused session to refuse the keystroke", || {
        b.ev(&fleet)
            .iter()
            .any(|e| e == &format!("refused sid={sid} face=term reason=holder"))
            .then_some(())
    });
    assert!(
        !b.verb(&format!("@{sid} text"))
            .rows()
            .join("\n")
            .contains("PAUSED"),
        "a keystroke landed on a session the bridge had paused"
    );
}
