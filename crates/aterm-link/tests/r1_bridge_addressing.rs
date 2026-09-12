// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **ROUND 1 — who a record says it is from, where its verdict goes, and what a
//! sender's own words are allowed to write down.**
//!
//! Every test here is over the real binaries. They are grouped because they are
//! one question asked five ways: a stranger's bytes choose part of an address or
//! a label, and the bridge is the wall that decides how much of that it repeats.

mod harness;

use harness::{until, World, FLEET};

/// **A node may attest one of its own SESSIONS, and nothing else.**
///
/// §4.1 permits exactly one attested exception — "a node writing on behalf of
/// one of its SESSIONS adds `from=<sid>`" — and §8.3's residual is "a node can
/// attribute a post to any of its OWN SESSIONS". `render_from` validated the
/// claim with `is_principal` alone, which also accepts `h-`, so a node holding
/// nothing but its own §8.2 ring could render `h-andrew@n-rogue`. Two consumers
/// key off that `h-` prefix: `hook::accepted`, which decides whether a row may
/// exit-2 a parked `Stop` hook, and `InboxRow::is_human`, which is the eviction
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

/// **A human's `control claim` moves the keyboard on the DOCUMENTED DEFAULT.**
///
/// `--accept-from` is empty by default, and `classify_kind` demoted any
/// `task`/`control` whose `<src>` was not literally on it. `deliver_record`
/// decides whether a record moves the keyboard from the CLASSIFIED kind, so a
/// human's `claim` became `kind=note demoted=control`, never reached
/// `decide_control`, and every following `term/in` under it was refused
/// `reason=holder`. §6.6 row 1 grants an `h-*` claim "unconditionally" and §9.3
/// makes the remote keyboard structural; §8.4's own spelling of the mitigation,
/// `--accept-from h-*`, is refused at startup because `*` is not in a principal.
/// Every handoff test in this crate boots with the human hard-coded onto the
/// allowlist, so nothing guarded the case the design actually describes.
#[test]
fn a_humans_claim_moves_the_keyboard_on_a_default_configuration() {
    let w = World::boot("r1claim", &[]);
    w.wait_ready();
    let (a, _b) = w.two_sessions();
    let epoch = until("the session's launch nonce", || {
        w.sessions()
            .into_iter()
            .find(|(_, sid, _)| *sid == a)
            .map(|(_, _, nonce)| nonce)
    });

    let mut god = w.god();
    let subject = format!("/f/{FLEET}/in/{}/{a}/h-andrew/control", w.node);
    god.publish(
        6_500,
        1,
        &subject,
        format!("v=1 t=1 epoch={epoch} text=claim").as_bytes(),
    )
    .expect("publish the claim");

    let row = format!("/f/{FLEET}/pub/{}/{a}/control", w.node);
    let stood = until("the human's claim to move the holder", || {
        let (rows, _) = w.god().last(&row, "", 8).ok()?;
        rows.into_iter()
            .find(|(_, s, _)| *s == row)
            .map(|(_, _, b)| String::from_utf8_lossy(&b).into_owned())
    });
    assert!(
        stood.contains("holder=h-andrew"),
        "§6.6 row 1 grants an h-* claim unconditionally: {stood}"
    );
    // AND IT IS STILL DELIVERED as a row: the agent must see that the human took
    // the wheel, and it arrives as what it is rather than demoted.
    let delivered = until("the claim to be delivered as a control row", || {
        w.inbox(&a)
            .into_iter()
            .find(|r| r.contains("from=h-andrew"))
    });
    assert!(
        delivered.contains("kind=control") && !delivered.contains("demoted="),
        "a human is accepted beside whatever --accept-from lists: {delivered}"
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
/// no operator undo (`aterm-link pin` is unimplemented). One message from a
/// prompt-injected worker permanently made a peer's sid `ERR ambiguous` for
/// every sender on the node.
#[test]
fn an_explicit_node_address_never_writes_the_pin_table() {
    let w = World::boot("r1pin", &[]);
    w.wait_ready();
    let (a, _b) = w.two_sessions();

    let posted = w.verb(&format!(
        "@{a} post to=@s-victim@n-attacker kind=note r1poison"
    ));
    assert!(posted.ok(), "{}", posted.header());

    // The post IS routed where it was addressed — this is a bound on what the
    // sender may WRITE DOWN, not a refusal to send.
    let lane = format!("/f/{FLEET}/in/n-attacker/s-victim/{}/note", w.node);
    until("the post to reach the address it named", || {
        let (rows, _) = w.god().last(&lane, "", 8).ok()?;
        rows.into_iter().find(|(_, s, _)| *s == lane).map(|_| ())
    });

    let pins = std::fs::read_to_string(w.state.join("pins")).unwrap_or_default();
    assert!(
        !pins.contains("s-victim"),
        "an address a sender typed is not an observation, and must not be \
         pinned: {pins:?}"
    );
}

/// **`post to=say` reaches the `say` face.**
///
/// The verb catalog, `POST_USAGE` and §11.2's serve spec all name `to=say`;
/// `cmd_post` accepts it and answers `OK <id>`; the tui renders and publishes
/// the face. `resolve_to` answered `Unroutable`, so every such post was retired
/// `off=- reason=unroutable` and vanished from `inbox` with no verdict a
/// non-waiting sender ever sees. aterm's catalog is its contract, so a verb row
/// promising a target that always fails is a first-rank defect.
#[test]
fn a_post_to_say_reaches_the_say_face() {
    let w = World::boot("r1say", &[]);
    w.wait_ready();
    let (a, _b) = w.two_sessions();

    let posted = w.verb(&format!("@{a} post to=say kind=note r1announced"));
    assert!(posted.ok(), "{}", posted.header());

    let face = format!("/f/{FLEET}/pub/{}/{a}/say/note", w.node);
    let said = until("the announcement to reach the say face", || {
        let (rows, _) = w.god().last(&face, "", 8).ok()?;
        rows.into_iter()
            .find(|(_, s, _)| *s == face)
            .map(|(_, _, b)| String::from_utf8_lossy(&b).into_owned())
    });
    assert!(said.contains("r1announced"), "{said}");
}

/// **A session's `ev` records live on the SESSION's own face.**
///
/// §3.3 makes `ev` a per-owner face and §10 says an applied `term/in` "leaves an
/// `ev` record `applied re=M seq=<n>` on the session's `ev` face". A3 published
/// every one of them on `…/node/ev` with the session named only inside the
/// pct-encoded payload — so `replay::session_of` answered `None`, the causal
/// pointer §10 asks for belonged to no partition in this crate's own
/// consistent-cut machinery and contributed no `CrossEdge`, and a reader scoped
/// to `ro:/f/<F>/pub/<n>/<sid>/>` could not see its own session's verdicts at
/// all. The check is `session_of` itself, so the test agrees with the replay
/// rather than with its own idea of a subject.
#[test]
fn a_sessions_ev_records_are_attributable_to_that_session() {
    let w = World::boot("r1evface", &[]);
    w.wait_ready();
    let _ = w.two_sessions();

    // A record for a session this node does not host: refused, recorded, and
    // the record names the session it was addressed to.
    let ghost = "s-r1ghost00000000";
    let subject = format!("/f/{FLEET}/in/{}/{ghost}/h-x/note", w.node);
    let (off, _) = w
        .god()
        .publish(6_800, 1, &subject, b"v=1 t=1 text=r1ghosted")
        .expect("publish to a session this node does not host");

    let face = format!("/f/{FLEET}/pub/{}/{ghost}/ev", w.node);
    let (rows, _) = until("the verdict to reach the session's own ev face", || {
        let (rows, page) = w.god().fetch(0, &face, 256).ok()?;
        rows.iter()
            .any(|(_, s, _)| *s == face)
            .then_some((rows, page))
    });
    let (_, subj, body) = rows
        .into_iter()
        .find(|(_, s, _)| *s == face)
        .expect("the row");
    let payload = aterm_link::body::Body::decode(&body).0;
    let ev = aterm_link::pct::decode(payload.unknown.get("ev").expect("an ev token"));
    assert!(
        ev.contains(&format!("off={off}")) && ev.contains("reason=not-hosted"),
        "{ev}"
    );
    assert_eq!(
        aterm_link::replay::session_of(&subj, &body).as_deref(),
        Some(ghost),
        "§10's causal record must belong to a partition the replay can name"
    );
}

/// **A refusal reaches the SENDER's lane, and never trips this node's own
/// forged-self alarm.**
///
/// §6.2 makes the per-sender quota safe by promising the sender a verdict "on
/// the sender's lane". A3 built that lane two ways and both were wrong for the
/// traffic that arrives: a peer node's message was answered at `p/<node>`, the
/// DIRECT-principal lane a node never drains, and a local session's was answered
/// on this node's own subtree under this node's own `<src>` — which came
/// straight back through this bridge's own group drain, was refused
/// `forged-self` because the publish offset was never registered, and published
/// `cap-compromised`: a false capability-compromise alarm the node raised
/// against itself, on a path an attacker triggers at will.
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

    // 1 — A PEER NODE, speaking for one of its sessions. The verdict belongs on
    // THAT session's lane on THAT node, which is the only owner path a bridge
    // drains — not on `p/<node>`.
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
        let (rows, _) = w.god().last(&peer_lane, "", 8).ok()?;
        rows.into_iter()
            .find(|(_, s, _)| *s == peer_lane)
            .filter(|(_, _, x)| String::from_utf8_lossy(x).contains("state=refused"))
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
    assert!(
        told.contains("state=refused") || told.contains("refused"),
        "{told}"
    );
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
    let w = World::boot("r1att", &[]);
    w.wait_ready();
    let (a, _b) = w.two_sessions();

    let set = w.verb(&format!("@{a} meta set attention needs a key"));
    assert!(set.ok(), "{}", set.header());
    let presence = format!("/f/{FLEET}/pub/{}/{a}/presence", w.node);
    let row = until("the escalation to reach the session's presence row", || {
        let (rows, _) = w.god().last(&presence, "", 8).ok()?;
        rows.into_iter()
            .find(|(_, s, _)| *s == presence)
            .map(|(_, _, b)| String::from_utf8_lossy(&b).into_owned())
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
        let (rows, _) = w.god().last(&ack, "", 8).ok()?;
        rows.into_iter()
            .find(|(_, s, _)| *s == ack)
            .map(|(_, _, b)| String::from_utf8_lossy(&b).into_owned())
            .filter(|b| b.contains(&format!("re={head}")))
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
