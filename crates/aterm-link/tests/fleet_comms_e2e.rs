// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **A7 — the composition rung** (`fleet.comms.e2e-exactly-once-across-nodes`).
//!
//! Every rung below this one proved one thing working alone. This file is the
//! one that asks whether they work TOGETHER, which is a different and harder
//! question: A2's ring, A3's bridge, A5's two sealed nodes, A6's idempotency key
//! and §6.6's handoff table, composed into the single exchange §12's A7 row
//! names —
//!
//! > an agent on node 1 asks, a human answers from a third client as `h-…`,
//! > takes control and drives a worker on node 2 through one `term/in` applied
//! > once, and the exchange replays from a consistent cut with both screens
//! > re-folded.
//!
//! ## "Applied once" is asserted across a crash, not on the happy path
//!
//! A3 could type a keystroke and see it once. That is not the claim. §6.5's last
//! row is about the window between "wrote the bytes" and "recorded that it wrote
//! them": a bridge that dies inside it either replays (and types twice) or does
//! not (and loses it, silently, which is what A3 did by resuming its drive face
//! at the head). A6 built the endpoint half — `feed-bin … id=<epoch>:<producer>:<seq>`
//! and a per-session, per-producer high-water mark — and said plainly that the
//! bridge half was missing. This file exercises the pair: the bridge journals
//! its intent and the key BEFORE the verb, `SIGKILL`s itself inside the window
//! from a named fault point, and the replacement asks the endpoint, with the same
//! key, which side of the window the keystroke fell on.
//!
//! ## No sleeps as synchronisation
//!
//! Every wait is `until <observable state>` — a record on the log, a field in a
//! reply, a file in a state dir. The bound is a HANG DETECTOR, not a timing
//! assertion. The crash is timed from INSIDE the bridge, by the process whose
//! progress defines the window, exactly as A3's two crash tests are.

#![cfg(unix)]

mod harness;

use harness::{until, Auditor, Fleet, Human, FLEET};

/// **A `term/in` is applied EXACTLY ONCE across a bridge that is `SIGKILL`ed
/// inside the feed window.**
///
/// The window, precisely: the intent is journalled, `feed-bin … id=<key>` has
/// answered `OK` — so the bytes are on the PTY — and nothing anywhere has yet
/// recorded that they are. `ATERM_LINK_FAULT=kill-in-feed-window` fires there,
/// once, with a real `SIGKILL`.
///
/// What must then be true, and none of it was true before this rung:
///
/// * aterm relaunches the bridge, and the new one finds `feeding` in the state
///   dir. A3's would have found nothing and resumed the drive face at the head,
///   so the record would never be seen again — the silent loss.
/// * the replay re-sends the SAME key. A6's mark says the sequence is already
///   consumed and answers `OK dup=1` **without writing**, so the screen still
///   holds one copy of the mark. A replay under a fresh key would hold two.
/// * the outcome is REPORTED: `ev applied … re=<M> … dup=1 replay=1`, carrying
///   `re=` as a body field so the causal edge is rebuildable from bytes alone
///   (§10).
/// * the session is drivable again at all. The bridge's death held every session
///   it had touched `reason=fabric-lost origin=fleet` (fail closed, §11.2), and
///   until the replacement reconciles that against the fleet's standing halt the
///   replay would answer `ERR halted` forever.
#[test]
fn a_keystroke_survives_a_bridge_killed_inside_the_feed_window_and_is_applied_once() {
    let mut fleet = Fleet::boot("a7feed");
    // The human must be on the allowlist or its `control` is demoted to a note
    // (§8.4) and never reaches the handoff table at all.
    let b = fleet.node_with(
        "a7feed-b",
        &["h-andrew"],
        &[],
        &[("ATERM_LINK_FAULT", "kill-in-feed-window")],
    );
    b.wait_ready(&fleet);
    let (sid, epoch) = b.session();
    let mut human = Human::arrive(&fleet, "h-andrew");

    // 1 — THE CLAIM. §6.6 row 1: a human's claim is granted unconditionally.
    human.control(&b.node, &sid, &epoch, "claim");
    let control_row = format!("/f/{FLEET}/pub/{}/{sid}/control", b.node);
    until("the human's claim to stand on the bus", || {
        let (rows, _) = fleet.god().last(&control_row, "", 8).ok()?;
        rows.into_iter()
            .find(|(_, s, _)| *s == control_row)
            .filter(|(_, _, x)| String::from_utf8_lossy(x).contains("holder=h-andrew"))
            .map(|_| ())
    });

    let pid_before = std::fs::read_to_string(b.state.join("pid")).expect("the bridge's pid");

    // 2 — THE KEYSTROKE. No newline: the shell ECHOES it onto the command line
    // and runs nothing, so one application is one copy on the screen and a
    // second application would be visible as a second copy.
    const MARK: &str = "A7XONCE";
    let gen = b.gen(&sid);
    let off = human.drive(&b.node, &sid, &epoch, &gen, None, MARK.as_bytes());

    // 3 — THE CRASH, from inside the window. The fault is ONE SHOT across
    // restarts (`fault-fired` in the state dir), so the replacement bridge runs
    // to completion rather than dying at the same step forever.
    until("the bridge to fire the fault and die", || {
        std::fs::read_to_string(b.state.join("fault-fired"))
            .ok()
            .map(|_| ())
    });
    until("aterm to relaunch the bridge under a new pid", || {
        let now = std::fs::read_to_string(b.state.join("pid")).ok()?;
        (now != pid_before).then_some(())
    });
    b.wait_ready(&fleet);

    // 4 — THE REPLAY RESOLVED IT, and said so. `dup=1` is the endpoint telling
    // the bridge the bytes had already landed; `replay=1` is the bridge saying
    // this verdict came from the journal rather than from a live record.
    let verdict = until("the replay to resolve the interrupted feed", || {
        b.ev(&fleet)
            .into_iter()
            .find(|e| e.starts_with("applied ") && e.contains("replay=1"))
    });
    assert!(
        verdict.contains(&format!("re={off}")) && verdict.contains("dup=1"),
        "the replay must resolve THIS record as already landed: {verdict}"
    );
    // The journal is retired. An entry that survived its own resolution would
    // replay on every attach forever — the silent retry §6.5 forbids.
    assert!(
        !b.state.join("feeding").exists(),
        "a resolved feed intent was left in the state dir"
    );

    // 5 — AND THE SCREEN HOLDS IT ONCE.
    let screen = b.verb(&format!("@{sid} text")).rows().join("\n");
    assert_eq!(
        screen.matches(MARK).count(),
        1,
        "a crash inside the feed window must not duplicate or lose the keystroke:\n{screen}"
    );

    // 6 — AND THE SESSION IS DRIVABLE AGAIN. The crash held it `fabric-lost`;
    // the replacement reconciled that against a fleet with no standing halt. A
    // bridge that could not lift its predecessor's fail-closed hold would leave
    // the fleet permanently un-drivable after any crash at all.
    until(
        "the fabric-lost hold to be lifted by the new bridge",
        || {
            b.verb(&format!("@{sid} status"))
                .header()
                .contains("hold=0")
                .then_some(())
        },
    );
    const SECOND: &str = "A7XAGAIN";
    let gen = b.gen(&sid);
    let second = human.drive(&b.node, &sid, &epoch, &gen, None, SECOND.as_bytes());
    until("a fresh keystroke to reach the PTY after the crash", || {
        b.verb(&format!("@{sid} text"))
            .rows()
            .join("\n")
            .contains(SECOND)
            .then_some(())
    });
    let applied = until("the fresh keystroke's own applied row", || {
        b.ev(&fleet)
            .into_iter()
            .find(|e| e.starts_with("applied ") && e.contains(&format!("re={second}")))
    });
    // §10's causal pointer, on the happy path: the offset it applied and the
    // `content_seq` baseline the screen that followed is measured from. A
    // `seq=-` would mean the screen could not be read, which is not this case.
    assert!(
        applied.contains(&format!("re={second}")) && !applied.contains("seq=-"),
        "an applied row must carry the record it applied and the baseline: {applied}"
    );
    assert!(!applied.contains("replay=1"), "{applied}");
}

/// **A consumed sequence is answered without writing, however it is asked.**
///
/// A6's mark is the endpoint's, not the bridge's, so it holds against ANY
/// connection that names the same key — which is what makes the bridge's replay
/// safe rather than merely lucky. This asks the second time from a plain Owner
/// connection: the socket an operator debugging a stuck driver would have,
/// speaking the same `feed-bin … id=` the bridge speaks.
///
/// The key is reconstructed the way the bridge mints it — the session's epoch,
/// the node's bound producer id, the record's own bus offset + 1 — so a test
/// that passes is a test that agreed with the bridge's arithmetic, not with its
/// own.
#[test]
fn a_consumed_sequence_is_answered_without_writing_however_it_is_asked() {
    let mut fleet = Fleet::boot("a7doubt");
    let b = fleet.node("a7doubt-b", &["h-andrew"]);
    b.wait_ready(&fleet);
    let (sid, epoch) = b.session();
    let mut human = Human::arrive(&fleet, "h-andrew");
    human.control(&b.node, &sid, &epoch, "claim");
    let control_row = format!("/f/{FLEET}/pub/{}/{sid}/control", b.node);
    until("the human's claim to stand on the bus", || {
        let (rows, _) = fleet.god().last(&control_row, "", 8).ok()?;
        rows.into_iter()
            .find(|(_, s, _)| *s == control_row)
            .filter(|(_, _, x)| String::from_utf8_lossy(x).contains("holder=h-andrew"))
            .map(|_| ())
    });

    // A keystroke the bridge applies normally, so the producer's mark exists and
    // its high-water is a real sequence.
    const MARK: &str = "A7XDOUBT";
    let gen = b.gen(&sid);
    let off = human.drive(&b.node, &sid, &epoch, &gen, None, MARK.as_bytes());
    until("the keystroke to reach the PTY", || {
        b.verb(&format!("@{sid} text"))
            .rows()
            .join("\n")
            .contains(MARK)
            .then_some(())
    });
    let applied = until("its applied row", || {
        b.ev(&fleet)
            .into_iter()
            .find(|e| e.starts_with("applied ") && e.contains(&format!("re={off}")))
    });
    assert!(
        !applied.contains("dup=1"),
        "a first apply is not a dup: {applied}"
    );

    // NOW THE SAME KEY AGAIN, from the outside. The bridge mints its key from
    // the record's own offset, so re-asking that exact sequence needs a second
    // path to the endpoint — a plain Owner connection, which is what an operator
    // debugging a stuck driver would have. The mark says APPLIED, so the answer
    // is `OK dup=1` and nothing is typed.
    let key = format!(
        "{epoch}:{}:{}",
        astream_cap::producer_id_of(&b.node),
        off + 1
    );
    let mut ctl = b.ctl();
    let reply = ctl
        .request_with_body(
            &format!("@{sid} feed-bin {} id={key}", MARK.len()),
            MARK.as_bytes(),
        )
        .expect("a second ask about the same sequence");
    assert!(
        reply.header().contains("dup=1"),
        "A6's mark must answer a consumed sequence without writing: {}",
        reply.header()
    );
    let screen = b.verb(&format!("@{sid} text")).rows().join("\n");
    assert_eq!(
        screen.matches(MARK).count(),
        1,
        "a duplicate key typed a second copy:\n{screen}"
    );
}

/// **A journalled feed whose record cannot be resolved is escalated ONCE, and
/// the journal is retired.**
///
/// §6.5's rule for an unknown outcome is "in-doubt, reported, never replayed",
/// and the reporting is the half A6 could not build: aterm-link sent no `id=`,
/// so it could never be told, and had nowhere to say it if it had been. This is
/// that half, driven through the one unresolvable case a test can stage
/// deterministically — a `feeding` entry naming an offset that holds no drive
/// record for this node.
///
/// The endpoint's own `ERR in-doubt seq=<n>` string reaches the same `ev` by the
/// same function; that mapping is pinned by `feed_verdict`'s unit test rather
/// than here, because the state it comes from (A6's mark settled UNKNOWN by an
/// attempt that failed AFTER the write could have happened) is reachable from
/// inside `aterm-gui` and from nowhere else. Saying which half is proved where
/// is worth more than a test that looked end-to-end and was not.
///
/// TWO properties, and the second is the one that makes it safe: the escalation
/// is published, and the entry is GONE afterwards. An intent that survived its
/// own resolution would be re-fed on every attach for the life of the node.
#[test]
fn an_unresolvable_feed_intent_is_escalated_once_and_the_journal_is_retired() {
    let mut fleet = Fleet::boot("a7esc");
    let mut b = fleet.node("a7esc-b", &["h-andrew"]);
    b.wait_ready(&fleet);
    let (sid, epoch) = b.session();

    // A journal entry for a record that is not there. The offset is far past the
    // head, so the re-read finds nothing and the outcome can never be known —
    // which is exactly the shape of a state dir that outlived the log it names.
    let ghost = 4_000_000_000u64;
    std::fs::write(
        b.state.join("feeding"),
        format!("off={ghost} sid={sid} key={epoch}:7:1\n"),
    )
    .expect("plant a feed intent");
    // Relaunch the WHOLE node: aterm mints a new session, so the sid the intent
    // names is not even hosted any more — the harshest form of unresolvable, and
    // the one a state dir that outlived its instance really produces.
    b.relaunch();
    b.wait_ready(&fleet);

    let escalation = until("the unresolvable intent to be escalated", || {
        b.ev(&fleet)
            .into_iter()
            .find(|e| e.starts_with("in-doubt ") && e.contains("reason=unreadable"))
    });
    assert!(
        escalation.contains(&format!("re={ghost}")) && escalation.contains("replay=1"),
        "the escalation must name the record it could not resolve: {escalation}"
    );
    until("the journal to be retired", || {
        (!b.state.join("feeding").exists()).then_some(())
    });

    // AND IT DOES NOT COME BACK. A second attach re-reads the state dir; a
    // retired entry means one escalation, not one per reconnect forever.
    let before = b
        .ev(&fleet)
        .into_iter()
        .filter(|e| e.starts_with("in-doubt "))
        .count();
    b.relaunch();
    b.wait_ready(&fleet);
    assert_eq!(
        b.ev(&fleet)
            .into_iter()
            .filter(|e| e.starts_with("in-doubt "))
            .count(),
        before,
        "a retired intent was escalated a second time"
    );
}

/// **THE WHOLE EXCHANGE, and its replay from a consistent cut.**
///
/// §12's A7 row, one test:
///
/// > an agent on node 1 asks, a human answers from a third client as `h-…`,
/// > takes control and drives a worker on node 2 through one `term/in` applied
/// > once, and the exchange replays from a consistent cut with both screens
/// > re-folded.
///
/// Every actor is real and none of them is the test process wearing a hat. The
/// agent is a session inside a shipped `aterm-gui` on node 1, posting through
/// its own `post` verb. The human is a THIRD CLIENT: a minted `h-andrew` ring
/// and one sealed connection, no aterm, no control socket — its every act is a
/// record, which is what §6.6's story B4 means by "a human's answer from a
/// phone". The worker is a session inside a different `aterm-gui`, on a
/// different node, reached only through the bus.
///
/// ## What the cut is over
///
/// §10: "With one broker per fleet the bus has a single offset spine, so a fleet
/// cut on the bus is one offset. Across the session logs the built
/// Chandy-Lamport machinery applies … over the sessions' logs joined by the
/// recorded edges — the composition claim of §12 (A7)."
///
/// So the partition is a SESSION, the cut is one bus offset per session, and the
/// edge that joins them is the human's `re=` — the offset of the agent's ask,
/// carried on the keystroke that answers it. The edges are rebuilt from the
/// stored bytes alone (`cross_edges_from_bus`), never from anything this test
/// remembered, and the consistency rule is asserted in BOTH directions: a cut
/// that admits the keystroke without the ask is an orphan and is rejected; one
/// that admits the ask without the keystroke is a message in flight and is
/// admitted.
///
/// ## The grade, stated
///
/// The re-folded screen is the last record on each session's `screen` face at or
/// below the cut — the opt-in snapshot face of §3.3, folded last-value. It is
/// NOT the byte-exact `term/out` re-fold: that is astream-host's own
/// `replay_to_cut` over envelope logs, already green as
/// `term.fleet.consistent-cut-replay` and `term.fleet.durable-watermark`, and
/// this rung neither re-proves nor re-implements it.
#[test]
fn an_ask_an_answer_and_a_drive_cross_two_nodes_and_replay_from_a_consistent_cut() {
    let mut fleet = Fleet::boot("a7comp");
    // NODE 1 hosts the agent; NODE 2 hosts the worker and publishes its screen.
    // Both accept the human, or its `control` is demoted to a note (§8.4) and
    // never reaches the handoff table.
    let n1 = fleet.node_with("a7comp-1", &["h-andrew"], &["--screen", "all"], &[]);
    let n2 = fleet.node_with("a7comp-2", &["h-andrew"], &["--screen", "all"], &[]);
    n1.wait_ready(&fleet);
    n2.wait_ready(&fleet);
    let (agent, agent_epoch) = n1.session();
    let (worker, worker_epoch) = n2.session();
    assert_ne!(n1.node, n2.node, "two nodes, two identities");
    let mut human = Human::arrive(&fleet, "h-andrew");

    // 1 — THE AGENT ASKS. Addressed to a PRINCIPAL, not a session: the human
    // has no session and no node, so its lane is `/f/<F>/in/p/h-andrew/…` and
    // the ask carries `from=<agent sid>` attested by node 1 (§4.1, §6.1).
    let ask = n1.verb(&format!(
        "@{agent} post to=h-andrew kind=ask --wait which%20branch%20holds%20the%20fixture"
    ));
    assert!(ask.ok(), "the ask: {} / {}", ask.header(), n1.log_tail());
    let ask_off: u64 = ask
        .header()
        .split_whitespace()
        .find_map(|t| t.strip_prefix("off="))
        .expect("a landed post answers off=")
        .parse()
        .expect("an offset is a number");

    // 2 — THE HUMAN READS IT, from a client that is not a node. The lane is the
    // human's own and the ask is on it, attested to the agent's session.
    let lane = until("the ask to reach the human's lane", || {
        human.lane().into_iter().find(|(off, _, _)| *off == ask_off)
    });
    let (_, subject, raw) = lane;
    assert!(subject.ends_with("/ask"), "{subject}");
    assert!(
        subject.starts_with(&format!("/f/{FLEET}/in/p/h-andrew/")),
        "a principal's lane, not a session's: {subject}"
    );
    let (body, _) = aterm_link::body::Body::decode(&raw);
    assert_eq!(
        body.from.as_deref(),
        Some(agent.as_str()),
        "the ask must be attested to the agent's session"
    );

    // 3 — THE HUMAN ANSWERS IT, back to the agent's own lane, correlated by the
    // broker offset nobody can forge.
    let answer_lane = format!("/f/{FLEET}/in/{}/{agent}/h-andrew/answer", n1.node);
    human.publish(
        &answer_lane,
        format!("v=1 t=1 re={ask_off} text=main").as_bytes(),
    );
    let row = until("the answer to reach the agent's inbox", || {
        n1.inbox(&agent)
            .into_iter()
            .find(|r| r.contains("kind=answer"))
    });
    assert!(row.contains(&format!("re={ask_off}")), "{row}");
    assert!(
        row.contains("trust=human"),
        "a human's answer is labelled as one: {row}"
    );

    // 4 — THE HUMAN TAKES CONTROL of the WORKER, on the OTHER node. §6.6 row 1:
    // granted unconditionally, and mirrored into aterm's own lease so a local
    // driver sees it.
    human.control(&n2.node, &worker, &worker_epoch, "claim");
    let control_row = format!("/f/{FLEET}/pub/{}/{worker}/control", n2.node);
    until("the claim to stand on the worker's control row", || {
        let (rows, _) = fleet.god().last(&control_row, "", 8).ok()?;
        rows.into_iter()
            .find(|(_, s, _)| *s == control_row)
            .filter(|(_, _, x)| String::from_utf8_lossy(x).contains("holder=h-andrew"))
            .map(|_| ())
    });
    until("aterm's own lease to mirror the fabric holder", || {
        n2.verb(&format!("@{worker} lease status"))
            .header()
            .contains("holder=fabric:h-andrew")
            .then_some(())
    });

    // 5 — AND DRIVES IT, ONCE. Bound to the epoch and the generation the human
    // read, and carrying `re=<the ask>` as CAUSALITY: this keystroke is what the
    // agent's question led to. No newline, so the shell echoes the mark and runs
    // nothing — one application is one copy on the screen.
    const MARK: &str = "A7XCOMPOSE";
    let gen = n2.gen(&worker);
    let drive_off = human.drive(
        &n2.node,
        &worker,
        &worker_epoch,
        &gen,
        Some(ask_off),
        MARK.as_bytes(),
    );
    until("the keystroke to reach the worker's PTY", || {
        n2.verb(&format!("@{worker} text"))
            .rows()
            .join("\n")
            .contains(MARK)
            .then_some(())
    });
    let applied = until("node 2 to record the apply with its causal pointer", || {
        n2.ev(&fleet)
            .into_iter()
            .find(|e| e.starts_with("applied ") && e.contains(&format!("re={drive_off}")))
    });
    assert!(
        applied.contains(&format!("sid={worker}")) && !applied.contains("seq=-"),
        "§10's `applied re=<M> seq=<n>`: {applied}"
    );
    // APPLIED ONCE. The same record, republished byte-for-byte under a fresh
    // producer sequence, is a second bus record with a DIFFERENT offset — so it
    // mints a different key, and the only thing that can refuse it is §6.6's
    // fence. The generation moved (the keystroke itself moved it), so it does.
    let replay_off = human.drive(
        &n2.node,
        &worker,
        &worker_epoch,
        &gen,
        Some(ask_off),
        MARK.as_bytes(),
    );
    until("the stale generation to be refused and recorded", || {
        n2.ev(&fleet)
            .iter()
            .any(|e| e == &format!("refused sid={worker} face=term reason=gen"))
            .then_some(())
    });
    let screen = n2.verb(&format!("@{worker} text")).rows().join("\n");
    assert_eq!(
        screen.matches(MARK).count(),
        1,
        "the keystroke was applied more than once:\n{screen}"
    );
    assert!(replay_off > drive_off);

    // 6 — THE CUT. Wait until both nodes have published a screen that shows the
    // exchange, then take the bus head: §10's "a fleet cut on the bus is one
    // offset", expanded into the per-session vector the edges are checked
    // against.
    until(
        "node 2's screen face to carry the applied keystroke",
        || {
            let subject = format!("/f/{FLEET}/term/{}/{worker}/screen", n2.node);
            let (rows, _) = fleet.god().last(&subject, "", 8).ok()?;
            rows.into_iter()
                .find(|(_, s, _)| *s == subject)
                .filter(|(_, _, b)| String::from_utf8_lossy(b).contains(MARK))
                .map(|_| ())
        },
    );
    until("node 1's screen face to exist at all", || {
        let subject = format!("/f/{FLEET}/term/{}/{agent}/screen", n1.node);
        let (rows, _) = fleet.god().last(&subject, "", 8).ok()?;
        rows.into_iter().find(|(_, s, _)| *s == subject).map(|_| ())
    });
    // THE READER IS AN AUDITOR, not the human. §8.2's human ring can halt the
    // fleet and drive a worker and cannot read another session's mail or any
    // screen; §10's replay is `ro:/f/<F>/>`, which is a different authority.
    let mut audit = Auditor::arrive(&fleet);
    let cut_off = audit.head().saturating_sub(1);
    let bus = audit.replay(cut_off);
    assert!(
        bus.iter().any(|(off, _, _)| *off == ask_off),
        "the replay must reach the ask"
    );

    // 7 — THE EDGES, REBUILT FROM THE BYTES. Nothing this test remembered is an
    // input: the offsets, the sessions and the `re=` all come off the records.
    let edges = aterm_link::replay::cross_edges_from_bus(&bus);
    let composition = aterm_link::replay::CrossEdge {
        cause: (agent.clone(), ask_off),
        effect: (worker.clone(), drive_off),
    };
    assert!(
        edges.contains(&composition),
        "the ask on node 1 must be a recorded cause of the keystroke on node 2:\n{edges:#?}"
    );

    // 8 — CONSISTENCY, BOTH WAYS.
    let mut cut = aterm_link::replay::Cut::flat(&bus, cut_off);
    assert!(
        aterm_link::replay::is_consistent(&cut, &edges),
        "the whole-bus cut must be consistent"
    );
    // AN ORPHAN: the keystroke admitted, the ask that caused it cut away.
    cut.set(&agent, ask_off - 1);
    assert!(
        !aterm_link::replay::is_consistent(&cut, &edges),
        "a cut that admits an effect without its cause is not a cut"
    );
    // IN FLIGHT: the ask admitted, the keystroke cut away. Admissible — that
    // asymmetry is the whole of Chandy-Lamport.
    cut.set(&agent, cut_off);
    cut.set(&worker, drive_off - 1);
    assert!(
        aterm_link::replay::is_consistent(&cut, &edges),
        "a cut that admits a cause without its effect is a message in flight"
    );

    // 9 — BOTH SCREENS RE-FOLDED, from the consistent cut, out of the bus bytes.
    let cut = aterm_link::replay::Cut::flat(&bus, cut_off);
    let worker_replay =
        aterm_link::replay::replay_to_cut(&cut, &worker, &bus).expect("fold the worker");
    let agent_replay =
        aterm_link::replay::replay_to_cut(&cut, &agent, &bus).expect("fold the agent");
    let worker_screen = worker_replay.screen.expect("the worker published a screen");
    assert!(
        worker_screen.contains(MARK),
        "the worker's re-folded screen must show the keystroke:\n{}",
        worker_screen.rows
    );
    assert!(
        agent_replay.screen.is_some(),
        "the agent's screen must re-fold too — both screens, not one"
    );
    // THE CONTROL FOLD, from the same cut: who held the keyboard, and where.
    assert_eq!(
        worker_replay.holder.as_deref(),
        Some("h-andrew"),
        "the replay must say the human held the worker's keyboard"
    );
    assert_eq!(
        agent_replay.holder, None,
        "and that nobody held the agent's — a claim on one session is not a claim on the fleet"
    );
    // AND THE EXCHANGE ITSELF. The ask is folded into the AGENT's partition
    // (attested `from=`), the answer into the agent's too, the keystroke into
    // the worker's. Three records, two sessions, one causal chain.
    assert!(
        agent_replay
            .records
            .iter()
            .any(|(off, _, _)| *off == ask_off),
        "the ask belongs to the agent's partition"
    );
    assert!(
        worker_replay
            .records
            .iter()
            .any(|(off, _, _)| *off == drive_off),
        "the keystroke belongs to the worker's partition"
    );
    assert!(
        !agent_replay
            .records
            .iter()
            .any(|(off, _, _)| *off == drive_off),
        "a keystroke on node 2 must not fold into node 1's session"
    );
    assert_eq!(agent_epoch.len(), 32, "an epoch is a 32-hex launch nonce");
}
