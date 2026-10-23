// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Two-node fabric integration tests on a shared broker ([`Fleet`], [`harness::World`]).

#![cfg(unix)]

use crate::harness;

use std::time::Duration;

use harness::{DEADLINE, FLEET, Fleet, until, until_within};

/// Wait for a last-value row to carry `what`, over the fleet's one shared read
/// connection ([`Fleet::read`]).
fn row_says(fleet: &Fleet, subject: &str, what: &str, budget: Duration) {
    until_within(budget, &format!("{subject} to say {what}"), || {
        fleet.row(subject).filter(|x| x.contains(what)).map(|_| ())
    });
}

/// **The whole A3 flow, across two nodes — and `ls` sees both.**
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

/// Two nodes advertising one sid: `post` answers `ERR ambiguous` and delivers
/// nothing.
#[test]
fn two_nodes_advertising_one_sid_make_post_answer_err_ambiguous() {
    let mut fleet = Fleet::boot("ambig");
    let a = fleet.node("ambig-a", &[]);
    let b = fleet.node("ambig-b", &[]);
    a.wait_ready(&fleet);
    b.wait_ready(&fleet);
    let (sid_a, _) = a.session();
    let (sid_b, _) = b.session();

    let presence = format!("/f/{FLEET}/pub/{}/{sid_b}/presence", b.node);
    row_says(&fleet, &presence, "state=live", DEADLINE);

    let first = a.verb(&format!(
        "@{sid_a} post to=@{sid_b} kind=note --wait pinning"
    ));
    assert!(first.ok(), "the pinning post: {}", first.header());
    until("the pin to reach A's state dir", || {
        let pins = std::fs::read_to_string(a.state.join("pins")).ok()?;
        pins.contains(&format!("{sid_b} {}", b.node)).then_some(())
    });
    let delivered_before = until("the pinning post to land at B", || {
        let rows = b.inbox(&sid_b);
        (!rows.is_empty()).then_some(rows.len())
    });

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

    let second = a.verb(&format!(
        "@{sid_a} post to=@{sid_b} kind=note --wait contested"
    ));
    assert!(
        second.header().starts_with("ERR ambiguous"),
        "a contested sid must answer `ERR ambiguous`, got {:?}\n{}",
        second.header(),
        a.log_tail()
    );
    until("the refusal to reach the node's own digest", || {
        a.ev(&fleet)
            .iter()
            .any(|e| e.contains("undeliverable") && e.contains("reason=ambiguous"))
            .then_some(())
    });

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

/// Observer mode delivers nothing, and never makes a session ambiguous.
#[test]
fn an_observer_delivers_nothing_and_never_makes_a_session_ambiguous() {
    let mut fleet = Fleet::boot("obs");
    let a = fleet.node("obs-a", &[]);
    let b = fleet.node("obs-b", &[]);
    a.wait_ready(&fleet);
    b.wait_ready(&fleet);
    let (sid_a, _) = a.session();
    let (sid_b, _) = b.session();

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
    struct Reaper(aterm_uds::spawnfd::Child);
    impl Drop for Reaper {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let _reaper = Reaper(child);

    until("the observer to announce itself", || {
        std::fs::read_to_string(&log)
            .ok()?
            .contains("OBSERVER MODE")
            .then_some(())
    });
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
