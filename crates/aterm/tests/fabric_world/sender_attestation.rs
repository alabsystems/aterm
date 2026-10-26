// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Sender attestation, verdict routing, and address pinning tests.

use crate::harness;

use harness::{FLEET, World, until};

/// **A node may attest one of its own SESSIONS, and nothing else.**
#[test]
fn a_node_cannot_attest_a_human_in_from() {
    let w = World::boot("r1from", &[]);
    w.wait_ready();
    let (a, _b) = w.two_sessions();

    let mut god = w.god();
    let subject = format!("/f/{FLEET}/in/{}/{a}/n-rogue/note", w.node);
    god.publish(6_400, 1, &subject, b"v=1 t=1 from=h-andrew text=r1forged")
        .expect("publish the forged attestation");
    god.publish(
        6_401,
        2,
        &subject,
        b"v=1 t=1 from=s-realsession text=r1honest",
    )
    .expect("publish the honest attestation");

    let rows = until("both notes to land", || {
        let rows = w.inbox(&a);
        (rows.iter().filter(|r| r.contains("text=r1")).count() >= 2).then_some(rows)
    });
    let forged = rows
        .iter()
        .find(|r| r.contains("text=r1forged"))
        .expect("the forged row landed");
    assert!(
        forged.contains("from=n-rogue") && !forged.contains("h-andrew"),
        "a node claiming to be a human is attributed to the NODE: {forged}"
    );
    assert!(forged.contains("trust=agent"), "{forged}");
    let honest = rows
        .iter()
        .find(|r| r.contains("text=r1honest"))
        .expect("the honest row landed");
    assert!(
        honest.contains("from=s-realsession@n-rogue"),
        "a node speaking for its own session is still attested: {honest}"
    );
}

/// **An unlisted principal's `answer` is NOT demoted.**
#[test]
fn an_unlisted_principals_answer_is_not_demoted() {
    let w = World::boot("r1answer", &[]);
    w.wait_ready();
    let (a, _b) = w.two_sessions();

    let subject = format!("/f/{FLEET}/in/{}/{a}/a-stranger/answer", w.node);
    w.god()
        .publish(6_600, 1, &subject, b"v=1 t=1 text=r1answered")
        .expect("publish the answer");

    let row = until("the answer to land", || {
        w.inbox(&a)
            .into_iter()
            .find(|r| r.contains("text=r1answered"))
    });
    assert!(
        row.contains("kind=answer") && !row.contains("demoted="),
        "an answer is answerable by whoever the asker asked: {row}"
    );
}

/// **A sender's own `post to=` argument never writes the TOFU pin table.**
#[test]
fn an_explicit_node_address_never_writes_the_pin_table() {
    let w = World::boot("r1pin", &[]);
    w.wait_ready();
    let (a, _b) = w.two_sessions();

    let nowhere = w.verb(&format!(
        "@{a} post to=@s-victim@n-nowhere kind=note --wait=60000 r1nowhere"
    ));
    assert!(
        nowhere.header().starts_with("ERR unroutable"),
        "a node the fleet never showed is no route: {}",
        nowhere.header()
    );
    let unminted = format!("/f/{FLEET}/in/n-nowhere/s-victim/{}/note", w.node);
    assert!(w.row(&unminted).is_none(), "no lane was minted for it");

    w.god()
        .publish(
            9_400,
            1,
            &format!("/f/{FLEET}/pub/n-attacker/node/presence"),
            b"v=1 t=1 state=live inc=1",
        )
        .expect("show n-attacker to the fleet");
    let shown = w.verb(&format!(
        "@{a} post to=@s-victim@n-attacker kind=note --wait=60000 r1unadvertised"
    ));
    assert!(
        shown.header().starts_with("ERR unroutable"),
        "a sid the named node does not advertise is no route: {}",
        shown.header()
    );
    w.god()
        .publish(
            9_400,
            2,
            &format!("/f/{FLEET}/pub/n-attacker/s-victim/presence"),
            b"v=1 t=1 state=live inc=1",
        )
        .expect("n-attacker advertises s-victim");

    let posted = w.verb(&format!(
        "@{a} post to=@s-victim@n-attacker kind=note r1poison"
    ));
    assert!(posted.ok(), "{}", posted.header());

    let lane = format!("/f/{FLEET}/in/n-attacker/s-victim/{}/note", w.node);
    until("the post to reach the address it named", || {
        w.row(&lane).map(|_| ())
    });

    let pins = std::fs::read_to_string(w.state.join("pins")).unwrap_or_default();
    assert!(
        !pins.contains("s-victim"),
        "an address a sender typed is not an observation, and must not be \
         pinned: {pins:?}"
    );
}

/// **`post to=say` reaches the `say` face — on the TOPIC, with the kind in the
/// body.**
#[test]
fn a_post_to_say_reaches_the_say_face() {
    let w = World::boot("r1say", &[]);
    w.wait_ready();
    let (a, _b) = w.two_sessions();

    let posted = w.verb(&format!("@{a} post to=say kind=note r1announced"));
    assert!(posted.ok(), "{}", posted.header());

    let face = format!("/f/{FLEET}/pub/{}/{a}/say/say", w.node);
    let said = until("the announcement to reach the say face", || w.row(&face));
    assert!(said.contains("r1announced"), "{said}");
    assert!(
        said.contains("kind=note"),
        "the kind rides in the body now, not the subject: {said}"
    );

    let posted = w.verb(&format!("@{a} post to=say:build.failed kind=note r1topic"));
    assert!(posted.ok(), "{}", posted.header());
    let face = format!("/f/{FLEET}/pub/{}/{a}/say/build.failed", w.node);
    let said = until("the topic face", || w.row(&face));
    assert!(
        said.contains("r1topic") && said.contains("kind=note"),
        "{said}"
    );

    for bad in ["say:", "say:Build", "say:.dot", "say:a/b", "say:has space"] {
        let r = w.verb(&format!("@{a} post to={bad} kind=note x"));
        assert!(
            r.header().starts_with("ERR usage"),
            "{bad} must be refused: {}",
            r.header()
        );
    }
}

/// **A session's `ev` records live on the SESSION's own face.**
#[test]
fn a_sessions_ev_records_are_attributable_to_that_session() {
    let w = World::boot("r1evface", &[]);
    w.wait_ready();
    let (a, _b) = w.two_sessions();

    let mut god = w.god();
    let hosted = format!("/f/{FLEET}/in/{}/{a}/h-x/note", w.node);
    let (refused, _) = god
        .publish(
            6_800,
            1,
            &hosted,
            b"v=1 t=1 via=NOT-A-PRINCIPAL text=r1refused",
        )
        .expect("publish a record the bridge refuses");
    let ghost = "s-r1ghost00000000";
    let subject = format!("/f/{FLEET}/in/{}/{ghost}/h-x/note", w.node);
    let (off, _) = god
        .publish(6_800, 2, &subject, b"v=1 t=1 text=r1ghosted")
        .expect("publish to a session this node does not host");

    let face = aterm_link::subject::session_face(FLEET, &w.node, &a, "ev");
    let ev = until("the verdict to reach the session's own ev face", || {
        w.ev_on(&face)
            .into_iter()
            .find(|e| e.contains(&format!("off={refused}")))
    });
    assert!(ev.contains("reason=via"), "{ev}");

    let node_face = aterm_link::subject::node_face(FLEET, &w.node, "ev");
    let ev = until("the not-hosted verdict to reach the node's ev face", || {
        w.ev_on(&node_face)
            .into_iter()
            .find(|e| e.contains(&format!("off={off}")))
    });
    assert!(
        ev.contains("reason=not-hosted") && ev.contains(&format!("sid={ghost}")),
        "{ev}"
    );
    let (rows, _) = w
        .god()
        .last(&format!("/f/{FLEET}/pub/{}/{ghost}/>", w.node), "", 8)
        .expect("read the ghost's subtree");
    assert!(
        rows.is_empty(),
        "a sid this node does not host became a subject: {rows:#?}"
    );
}

/// **A refusal reaches the SENDER's lane, and never trips this node's own
/// forged-self alarm.**
#[test]
fn a_refusal_reaches_the_senders_own_lane_and_raises_no_false_alarm() {
    let w = World::boot("r1notify", &[]);
    w.wait_ready();
    let (a, b) = w.two_sessions();

    let huge = aterm_link::pct::encode(&"z".repeat(300_000));
    let mut god = w.god();

    let from_peer = format!("/f/{FLEET}/in/{}/{a}/n-peer/note", w.node);
    god.publish(
        6_700,
        1,
        &from_peer,
        format!("v=1 t=1 from=s-peerside text={huge}").as_bytes(),
    )
    .expect("publish the peer's oversized note");
    let peer_lane = format!("/f/{FLEET}/in/n-peer/s-peerside/{}/undeliverable", w.node);
    until("the peer's own lane to carry the verdict", || {
        w.row(&peer_lane)
            .filter(|x| x.contains("not%20delivered"))
            .map(|_| ())
    });

    let from_local = format!("/f/{FLEET}/in/{}/{b}/{a}/note", w.node);
    god.publish(
        6_701,
        2,
        &from_local,
        format!("v=1 t=1 text={huge}").as_bytes(),
    )
    .expect("publish the local oversized note");
    let told = until("the local sender to be told on its own lane", || {
        w.inbox(&a)
            .into_iter()
            .find(|r| r.contains("kind=undeliverable"))
    });
    assert!(told.contains("not%20delivered"), "{told}");
    let ev = w.ev();
    assert!(
        !ev.iter().any(|e| e.starts_with("cap-compromised")),
        "a node must not report ITSELF compromised over a verdict it sent: {ev:#?}"
    );
    assert!(
        !ev.iter().any(|e| e.contains("reason=forged-self")),
        "our own verdict is not a forgery of ourselves: {ev:#?}"
    );
}

/// **`attention=` has a writer, and the halt ack has a bound.**
#[test]
fn attention_is_published_and_the_halt_ack_costs_one_subject() {
    let w = World::boot_with("r1att", &[], &[harness::FAST_TIMERS]);
    w.wait_ready();
    let (a, _b) = w.two_sessions();

    let set = w.verb(&format!("@{a} meta set attention needs a key"));
    assert!(set.ok(), "{}", set.header());
    let presence = format!("/f/{FLEET}/pub/{}/{a}/presence", w.node);
    let row = until("the escalation to reach the session's presence row", || {
        w.row(&presence)
            .filter(|b| b.contains("attention=") && !b.contains("attention=-"))
    });
    assert!(row.contains("needs"), "the row carries the words: {row}");
    assert!(
        !row.contains("fabric="),
        "a session row must not claim a reachability it cannot know: {row}"
    );

    let mut god = w.god();
    let halt = format!("/f/{FLEET}/fleet/h-x/halt");
    for (seq, state) in [(1u64, "on"), (2, "off"), (3, "on")] {
        god.publish(
            5_100,
            seq,
            &halt,
            format!("v=1 t=1 state={state} reason=r1").as_bytes(),
        )
        .expect("publish a halt record");
    }
    let last = format!("/f/{FLEET}/fleet/h-x/halt");
    let (rows, _) = w.god().last(&last, "", 8).expect("read the halt back");
    let head = rows
        .iter()
        .find(|(_, s, _)| *s == last)
        .map(|(off, _, _)| *off)
        .expect("the last halt record");
    let ack = format!("/f/{FLEET}/pub/{}/node/ack", w.node);
    until("the node to answer the newest barrier", || {
        w.row(&ack).filter(|b| b.contains(&format!("re={head}")))
    });

    let (subjects, _) = w
        .god()
        .last(&format!("/f/{FLEET}/pub/{}/>", w.node), "", 512)
        .expect("read the node's own subtree");
    let acks: Vec<String> = subjects
        .into_iter()
        .map(|(_, s, _)| s)
        .filter(|s| s.contains("/node/ack"))
        .collect();
    assert_eq!(
        acks.len(),
        1,
        "the ack face must cost the node's producer ONE subject, whatever the \
         fleet's halt history: {acks:#?}"
    );
}
