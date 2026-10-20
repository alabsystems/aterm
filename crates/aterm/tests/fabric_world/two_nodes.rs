// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **Two nodes on one broker**: what A3 proved on one node, proved again
//! with the two peers in DIFFERENT `aterm` processes with different node
//! identities, on one guarded broker's Unix socket ([`Fleet`]). Nothing
//! here is a mock — the endpoints are shipped `aterm`s, the bridges are
//! shipped `aterm link serve` children they launched over inherited
//! socketpairs, and the broker enforces capabilities on every attach.
//!
//! What these need is two NODES, which one [`harness::World`] cannot be: a
//! bare `@s-<sid>` resolved from the roster and pinned, a second claimant
//! for a pinned sid (§6.1's `ERR ambiguous`), and an observer that
//! advertises sessions it does not host (§11.2). The fabric is one machine's
//! bus, and these are two aterms on one machine.
//!
//! ## No sleeps as synchronisation
//!
//! Every wait is `until <observable state>`: a record on the log, a field in a
//! reply, a process's own output. The bound is a HANG DETECTOR.

#![cfg(unix)]

use crate::harness;

use std::time::Duration;

use harness::{DEADLINE, FLEET, Fleet, until, until_within};

/// Wait for a last-value row to carry `what`, over the fleet's one shared read
/// connection ([`Fleet::read`]).
///
/// `budget` is stated by the caller, because the two classes of wait here are
/// not the same: most rows are published by the arrival that caused them, and a
/// few by the bridge's periodic backstop (see [`harness::until_within`]).
fn row_says(fleet: &Fleet, subject: &str, what: &str, budget: Duration) {
    until_within(budget, &format!("{subject} to say {what}"), || {
        fleet.row(subject).filter(|x| x.contains(what)).map(|_| ())
    });
}

/// **The whole A3 flow, across two nodes — and `ls` sees both.**
///
/// A's session posts to B's session by its bare `@s-<sid>`; A's bridge has
/// never heard of that sid, so it resolves it the way §6.1 says — from the
/// roster, pinned on first sight — publishes under its own bound cap, and B's
/// bridge picks it off its own durable group and delivers it. B answers `re=`
/// the offset A was told, and A's inbox resolves that back to A's own post id.
///
/// The offsets are the broker's, so the correlation id nobody can forge is the
/// same one across two nodes as it was across two sessions.
#[test]
fn a_post_crosses_two_nodes_and_ls_sees_both() {
    let mut fleet = Fleet::boot("cross");
    let a = fleet.node("cross-a", &[]);
    let b = fleet.node("cross-b", &[]);
    a.wait_ready(&fleet);
    b.wait_ready(&fleet);
    let (sid_a, _) = a.session();
    let (sid_b, _) = b.session();
    assert_ne!(a.node, b.node, "two nodes, two identities");

    // `ls` FIRST, before any traffic: the roster is presence, not history.
    // Both nodes' sessions must be listed by ONE `Last` round trip from a
    // process that hosts nothing (§7). §7'S ROW IS `<node> <host> <sid> …`, so
    // the node and the session are the FIRST and THIRD columns with the host
    // between them — a session row has no `host=` of its own and prints `-`
    // there (`cli.rs`'s module header says which columns have no writer yet
    // and why).
    let names = |out: &str, node: &str, sid: &str| {
        out.lines().any(|l| {
            let c: Vec<&str> = l.split_whitespace().collect();
            c.first() == Some(&node) && c.get(2) == Some(&sid)
        })
    };
    let listing = until("aterm link ls to list both nodes' sessions", || {
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
        let generation = row
            .split_whitespace()
            .find_map(|t| t.strip_prefix("gen="))
            .unwrap_or("-");
        assert!(
            generation.contains(':') && generation != "-",
            "a session row must carry `<content_seq>:<fp16>`: {row}"
        );
    }

    // THE POST. `--wait` answers the broker offset the record landed at.
    let reply = a.verb(&format!(
        "@{sid_a} post to=@{sid_b} kind=ask --wait A5 hello"
    ));
    assert!(
        reply.ok(),
        "post across two nodes: {} / {}",
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

    // AND A SECOND BARE POST. The first one's walk asked A's bridge for its
    // live presence view (`aterm_link::presence_view`), and this one is routed
    // through it if it is up — one read of what landed since — or by the walk
    // if it is not. Nothing out here can tell which: the claim is only that
    // the route is the same either way. That the view goes live under a node's
    // own ring on a guarded broker, and that its confirming read passes the
    // guard, is proved where the view can be seen: aterm-link's
    // `the_presence_view_goes_live_under_a_nodes_ring_on_a_guarded_broker`.
    let again = a.verb(&format!(
        "@{sid_a} post to=@{sid_b} kind=note --wait A5 again"
    ));
    assert!(
        again.ok(),
        "the second bare post: {} / {}",
        again.header(),
        a.log_tail()
    );
    let row = until("the second post to land in B's inbox", || {
        b.inbox(&sid_b)
            .into_iter()
            .find(|r| r.contains("kind=note"))
    });
    assert!(
        row.contains(&format!("from={sid_a}@{}", a.node)),
        "B must see A's SESSION again: {row}"
    );
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
    row_says(&fleet, &presence, "state=live", DEADLINE);

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
    let (mut client, _closer) = aterm_link::transport::connect(&fleet.sock, DEADLINE)
        .expect("the rogue reaches the broker on the same socket");
    client
        .attach_all(
            caps.iter()
                .map(|cap| (cap.grant.as_str(), cap.tag.as_slice())),
        )
        .expect("the rogue's cap attaches — it is a real capability");
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
/// A hand-started `aterm link serve --sock … --token-file …` holds an ordinary
/// Owner token, so it is not the bridge: §11.2 gives it "presence and `ev` only,
/// no delivery, no hold".
///
/// The second half is the interaction that needs two nodes. An observer
/// publishes presence rows for sessions it WATCHES and does not host. §6.1
/// routes a bare `@s-<sid>` only when exactly one node advertises it.
/// Unmarked, opening a read-only observer would therefore make every session
/// it could see ambiguous across the bus — a working fabric broken by a tool
/// that writes nothing. The rows carry `observer=1` and the resolver skips
/// them, so this test posts across the nodes WITH an observer running.
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
    // `spawnfd`'s: the observer holds its stdio and nothing else of this
    // process, not another test thread's pipe for as long as it serves.
    let mut observer = aterm_uds::spawnfd::command(harness::aterm_to_launch());
    let child = harness::scratch_spawnfd(&mut observer, &fleet.tmp)
        .args(["link", "serve", "--fleet", FLEET, "--broker", &fleet.sock])
        .args([
            "--cap-file",
            cap_path.to_string_lossy().as_ref(),
            "--state",
            state.to_string_lossy().as_ref(),
            "--sock",
            &b.ctl_sock,
            "--token-file",
            token_file.to_string_lossy().as_ref(),
        ])
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .stdout(out)
        .stderr(err)
        .spawn()
        .expect("hand-start an observer");
    // Killed on every exit path, panics included: nothing here is supervised,
    // so a leaked observer would outlive the test and keep publishing.
    struct Reaper(aterm_uds::spawnfd::Child);
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
    let observer_row = format!("/f/{FLEET}/pub/{watcher}/{sid_b}/presence");
    row_says(&fleet, &observer_row, "observer=1", DEADLINE);
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
        let (rows, _) = fleet.read(|c| c.fetch(0, &ev_face, 256)).ok()?;
        rows.iter()
            .any(|(_, _, x)| String::from_utf8_lossy(x).contains("observer"))
            .then_some(())
    });
    assert!(
        b.inbox(&sid_b).is_empty(),
        "an observer delivered a row: {:?}",
        b.inbox(&sid_b)
    );

    // AND THE BUS STILL ROUTES. The observer advertises B's session under its
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
