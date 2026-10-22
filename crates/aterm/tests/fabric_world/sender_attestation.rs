// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **Who a record says it is from, where its verdict goes, and what a
//! sender's own words are allowed to write down.**
//!
//! Every test here is over the real binaries. They are grouped because they are
//! one question asked five ways: a stranger's bytes choose part of an address or
//! a label, and the bridge is the wall that decides how much of that it repeats.

use crate::harness;

use harness::{FLEET, World, until};

/// **A node may attest one of its own SESSIONS, and nothing else.**
///
/// §4.1 permits exactly one attested exception — "a node writing on behalf of
/// one of its SESSIONS adds `from=<sid>`" — and §8.3's residual is "a node can
/// attribute a post to any of its OWN SESSIONS". `render_from` validated the
/// claim with `is_principal` alone, which also accepts `h-`, so a node holding
/// nothing but its own §8.2 ring could render `h-andrew@n-rogue`. One consumer
/// keys off that `h-` prefix: `InboxRow::is_human`, which is the eviction
/// order §6.2 promises "never evicts an `h-*` row ahead of anyone else's".
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
    // The one attestation the design DOES allow still works, so this is a wall
    // rather than a blanket refusal.
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
///
/// §8.4's allowlist covers `{task, control}` and this crate said so twice, in
/// two files, with two different lists: `bridge::DEMOTE_UNLESS_ACCEPTED` (the
/// one that runs) and a dead `pub body::ACCEPTED_ONLY_KINDS` that also named
/// `answer`. An `answer`'s authority is the `re=` the receiver itself minted, so
/// demoting one would break request/reply for every peer not on the allowlist.
/// The dead constant is gone; this is the behaviour it misdescribed.
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
///
/// §6.1 makes the pin a defence against T15: a bare `@s-<sid>` routes only when
/// exactly one node advertises it "and the sender's bridge has pinned that (sid
/// → node) pair on first sight" — first sight being a presence row the claiming
/// node held a cap to publish. A3 also pinned on the EXPLICIT-node arm, where
/// the node segment is a string a session typed: `post` is owner-class, every
/// in-session `aterm-ctl @self` is Owner (§8.1), and the table is durable with
/// deleting the file as its only undo (`aterm-link pin` was cut unbuilt in
/// 622e40af6). One message from a
/// prompt-injected worker permanently made a peer's sid `ERR ambiguous` for
/// every sender on the node.
///
/// And the node segment must name a node the fleet has SHOWN (r3-08): a node
/// with no presence row is a name the sender made up, and routing it would
/// mint a lane nobody drains out of this node's lifetime subject budget — so
/// the post is retired `unroutable` and nothing is minted. `n-attacker` is
/// shown first, by its own presence row, as the attacker's real node would be.
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

    let posted = w.verb(&format!(
        "@{a} post to=@s-victim@n-attacker kind=note r1poison"
    ));
    assert!(posted.ok(), "{}", posted.header());

    // The post IS routed where it was addressed — this is a bound on what the
    // sender may WRITE DOWN, not a refusal to send.
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
///
/// retired the per-kind leaf: `say/<topic>` costs one subject per
/// topic, where `say/<topic>/<kind>` would have cost one per topic AND kind on
/// a budget this round has not reclaimed. Bare `to=say` is the topic `say`, so
/// every address that worked before still does — only the subject's last
/// segment changed meaning, and the kind moved into the record.
///
/// The verb catalog, `POST_USAGE` and §11.2's serve spec all name `to=say`;
/// `cmd_post` accepts it and answers `OK <id>`. `resolve_to` answered
/// `Unroutable`, so every such post was retired `off=- reason=unroutable` and
/// vanished from `inbox` with no verdict a non-waiting sender ever sees.
/// aterm's catalog is its contract, so a verb row promising a target that
/// always fails is a first-rank defect.
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

    // AND A NAMED TOPIC gets its own last segment.
    let posted = w.verb(&format!("@{a} post to=say:build.failed kind=note r1topic"));
    assert!(posted.ok(), "{}", posted.header());
    let face = format!("/f/{FLEET}/pub/{}/{a}/say/build.failed", w.node);
    let said = until("the topic face", || w.row(&face));
    assert!(
        said.contains("r1topic") && said.contains("kind=note"),
        "{said}"
    );

    // A TOPIC THAT IS NOT ONE IS A USAGE ERROR, at the post — not a record
    // retired `unroutable` a publish later, which a non-waiting sender never
    // sees.
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
///
/// §3.3 makes `ev` a per-owner face and §10 says an applied `term/in` "leaves an
/// `ev` record `applied re=M seq=<n>` on the session's `ev` face". A3 published
/// every one of them on `…/node/ev` with the session named only inside the
/// pct-encoded payload — so the causal pointer §10 asks for belonged to no
/// partition any reader could name, and a reader scoped to
/// `ro:/f/<F>/pub/<n>/<sid>/>` could not see its own session's verdicts at all.
///
/// THE ORACLE IS THE CRATE'S OWN SUBJECT BUILDER, so the test agrees with the
/// code rather than with its own idea of a subject: `subject::session_face` is
/// the function that MINTS this subject in the bridge.
///
/// A SID THIS NODE DOES NOT HOST HAS NO FACE HERE. The sender chooses that sid,
/// and every distinct one on `…/<sid>/ev` would be a new subject on the node's
/// distinct-subject budget, which nothing reclaims. Its refusal is recorded on
/// the node's face, naming the sid.
#[test]
fn a_sessions_ev_records_are_attributable_to_that_session() {
    let w = World::boot("r1evface", &[]);
    w.wait_ready();
    let (a, _b) = w.two_sessions();

    // A record refused for a session this node hosts (a relay chain that is
    // not one), and a record for a session it does not host.
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
///
/// §6.2 makes the per-sender quota safe by telling the sender "on the sender's
/// lane". On another node that lane is opened within the bridge's
/// `NEW_LANES_PER_NODE` for that node, so a first contact is answered and a
/// peer rotating `from=` past the bound is recorded `unnotified` instead. A3
/// built that lane two ways and both were wrong for the traffic that arrives: a
/// peer node's message was answered at `p/<node>`, the DIRECT-principal lane a
/// node never drains, and a local session's was answered on this node's own
/// subtree under this node's own `<src>` — which came straight back through
/// this bridge's own group drain, was refused `forged-self` because the publish
/// offset was never registered, and published `cap-compromised`: a false
/// capability-compromise alarm the node raised against itself, on a path an
/// attacker triggers at will.
///
/// The refusal used here is `ERR too large` rather than `ERR quota` because it
/// is one record instead of sixty-five; the verdict path is the same one.
#[test]
fn a_refusal_reaches_the_senders_own_lane_and_raises_no_false_alarm() {
    let w = World::boot("r1notify", &[]);
    w.wait_ready();
    let (a, b) = w.two_sessions();

    // Past the endpoint's own `BODY_MAX`, so `deliver` answers `ERR too large`.
    let huge = aterm_link::pct::encode(&"z".repeat(300_000));
    let mut god = w.god();

    // 1 — A PEER NODE, speaking for one of its sessions, at first contact. The
    // verdict belongs on THAT session's lane on THAT node, which is the only
    // owner path a bridge drains — not on `p/<node>`.
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

    // 2 — A LOCAL SESSION. Its verdict lands on this node's own lane, which is
    // correct, and the offset must be remembered as ours BEFORE the group drain
    // hands it back — or the bridge reports itself compromised.
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
///
/// Two findings, one boot, because both are about what the node PUBLISHES about
/// itself over time.
///
/// A10's `notify --on attention`, A8's glance column and §9.3's escalation story
/// all read `attention=` off a presence row, and nothing in the fabric ever
/// wrote it: every one of those readers had no writer, and the A10 tests only
/// passed because they synthesised presence rows by hand.
///
/// The halt ack used one SUBJECT per barrier. The broker bounds a producer at
/// 4096 distinct subjects, rebuilt from the log on every open, and a node's
/// producer id outlives every restart — so a fleet that toggled its halt two
/// thousand times permanently spent the node's whole budget, after which even
/// its own `live` presence row is refused and no bridge on it can attach again.
#[test]
fn attention_is_published_and_the_halt_ack_costs_one_subject() {
    // The escalation lands inside the republish window of the session's first
    // row, so the next round carries it: at test pace ([`harness::FAST_TIMERS`]).
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
    // AND `fabric=` IS GONE from a session row. It was the literal `connected`
    // with no will behind it, so a dead node's session rows all claimed to be
    // reachable while its own node row said `gone`. `glance` names the field one
    // of the two a human actually reads and defines it as node reachability; a
    // constant that cannot answer that is worse than an absent token, which
    // `GUARANTEED` renders as `-` — "unknown", which is the truth.
    assert!(
        !row.contains("fabric="),
        "a session row must not claim a reachability it cannot know: {row}"
    );

    // THREE BARRIERS, ONE SUBJECT. A3 spent one per halt record, on and off.
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
