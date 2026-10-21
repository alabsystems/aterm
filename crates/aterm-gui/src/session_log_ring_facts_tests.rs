// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE LOG HOLDS EVERY FACT THE RING HOLDS (session log phase 6, 6F-5 to
//! 6F-7): each ring row's writer writes, in the row's own hold, a mark (or a
//! record's fields) the row's whole payload reads from. Here: each new
//! writer's mark wakes a parked `await log` (a one-minute hang detector),
//! and what is content tier (a topic's name, a reason, a closer, a reply)
//! is in RAM and in no record that leaves it — the journal, the carry, a
//! bus body, `brief`, or `log` below `tier=content` (rulings U11-U13). The
//! row-for-record equalities are `session_log_overlap_tests`'.

use std::sync::Arc;

use aterm_slog::Tier;

use super::overlap_tests::{fetch_answered, link_up, queued};
use super::tests::{fabric_rig, parked_log_await, returned};
use crate::session_store::{ExitActor, ExitReason, SessionHandle, SessionState};

/// Every place `h`'s log is written below the content tier: `log` at the
/// host and wire tiers, the journal's record frames, the carry's lines, a
/// bus body of every record (at the meta tier, the most the bus carries:
/// what the mirror publishes of a bus-class record, and what it would of
/// any other) and `brief`. Each line is tagged with where it is.
pub(super) fn below_content(h: &SessionHandle) -> Vec<(&'static str, String)> {
    below_content_of(h, true)
}

/// [`below_content`], the carry's lines only when `carried` (a closed
/// session is carried no more).
fn below_content_of(h: &SessionHandle, carried: bool) -> Vec<(&'static str, String)> {
    let mut g = h.ctx.timeline.lock().unwrap();
    let sid = aterm_slog::ShortId::new(h.ctx.self_id.as_str()).expect("a sid");
    let mut out = Vec::new();
    let ep = g.slog().log().ep();
    for r in g.slog().log().window() {
        for (what, tier) in [("log host", Tier::Host), ("log wire", Tier::Wire)] {
            out.push((what, aterm_slog::encode(r, tier).expect("fits")));
        }
        let frame = aterm_slog::journal::Frame::Rec {
            sid,
            rec: r.clone(),
            restated: false,
        };
        out.push((
            "journal",
            aterm_slog::journal::encode_frame(&frame).expect("a journal frame"),
        ));
        out.push((
            "bus",
            aterm_slog::encode_bus(r, Tier::WireMeta, ep, None).expect("a bus body"),
        ));
    }
    let now = g.slog().now();
    out.push((
        "brief",
        super::super::render_brief(
            g.slog().log(),
            h.ctx.self_id.as_str(),
            aterm_slog::Node::SESSION,
            None,
            aterm_slog::DEFAULT_BUDGET,
            now.wall_ms,
        ),
    ));
    if carried {
        let carry = g
            .slog_mut()
            .carry_source()
            .encode(aterm_slog::CarryBudget::SESSION)
            .expect("carries");
        g.slog_mut().lift_read_fence();
        out.extend(carry.lines.into_iter().map(|l| ("carry", l)));
    }
    out
}

/// `h`'s log at the content tier, one line a record.
fn content(h: &SessionHandle) -> String {
    super::tests::content_lines(&h.ctx.timeline).join("\n")
}

/// EACH RING-ONLY FACT'S WRITER WAKES THE LOG'S READERS (6F-5): the
/// registry's state move, the policy cell's move, a topic added and
/// dropped, a parked fetch and a lost bridge's advisory, each through its
/// production entry point, each site's parked `await log kind=<k>` its
/// one-minute hang detector: `mark - state state=spawning|alive`, `mark - policy
/// gen=7`, `mark - topic change=add|drop fp=`, `mark - mail-fetch off=7`,
/// `mark - advisory origin=fleet gen=`, `log`'s host tier printing no
/// topic name and no reason. NEGATIVE CONTROLS: a state set again and a
/// topic added again move nothing and write no mark.
#[test]
fn each_ring_only_fact_wakes_a_parked_await_log() {
    let (h, store) = fabric_rig(71);
    // The registry's moves that leave the session running (an exited
    // session's parked reads end `ERR exited`): back to spawning (a test
    // handle is alive), the same again, then its reader's first read.
    let rx = parked_log_await(&h, &store, "state");
    store.write().unwrap().set_state(71, SessionState::Spawning);
    let got = returned(&rx, "set_state");
    assert!(got.contains(" mark - state state=spawning "), "{got}");
    store.write().unwrap().set_state(71, SessionState::Spawning);
    let lines = content(&h);
    assert_eq!(lines.matches(" mark - state ").count(), 1, "{lines}");
    let rx = parked_log_await(&h, &store, "state");
    assert!(store.write().unwrap().mark_alive(71));
    let got = returned(&rx, "mark_alive");
    assert!(got.contains(" mark - state state=alive "), "{got}");
    let rx = parked_log_await(&h, &store, "policy");
    crate::harness_policy_verb::record_policy_move(&h.ctx.timeline, 7);
    let got = returned(&rx, "policy");
    assert!(got.contains(" mark - policy gen=7 "), "{got}");
    let rx = parked_log_await(&h, &store, "topic");
    assert_eq!(
        h.ctx.fabric.topic_add("builds", "@12", &h.ctx.timeline),
        Ok(true)
    );
    let got = returned(&rx, "topic add");
    assert!(
        got.contains(" mark - topic change=add fp=") && !got.contains("builds"),
        "{got}"
    );
    assert_eq!(
        h.ctx.fabric.topic_add("builds", "@99", &h.ctx.timeline),
        Ok(false)
    );
    let rx = parked_log_await(&h, &store, "topic");
    assert!(h.ctx.fabric.topic_drop("builds", &h.ctx.timeline));
    let got = returned(&rx, "topic drop");
    assert!(got.contains(" mark - topic change=drop fp="), "{got}");
    let lines = content(&h);
    assert_eq!(lines.matches(" mark - topic ").count(), 2, "{lines}");
    crate::fabric::with_link_reset(|| {
        let (h, store) = fabric_rig(72);
        let generation = link_up(&store);
        let rx = parked_log_await(&h, &store, "mail-fetch");
        fetch_answered(&h, &store, 7);
        let got = returned(&rx, "inbox get @7");
        assert!(got.contains(" mark - mail-fetch off=7 "), "{got}");
        let line = format!(
            "{} off=8 from=h-andrew kind=note trust=human text=SENTINEL",
            h.ctx.self_id.as_str()
        );
        assert_eq!(
            crate::fabric::cmd_deliver_waking(&store, &line, &|_| {}),
            "OK 1\n"
        );
        let rx = parked_log_await(&h, &store, "advisory");
        assert_eq!(crate::fabric::bridge_lost(&store, generation), 1);
        let got = returned(&rx, "bridge lost");
        assert!(
            got.contains(" mark - advisory origin=fleet gen=")
                && got.contains(" by=aterm ")
                && !got.contains("reason="),
            "{got}"
        );
    });
}

/// T14, AMENDED (ruling U11): A TOPIC IS A FINGERPRINT WHEREVER ITS RECORD
/// LEAVES RAM. A planted topic name and its `since=` token are on the
/// `topic` mark at the content tier (beside the name's `fp=`), and in no
/// journal frame, no carry line, no bus body, no `brief` and no `log` line
/// below `tier=content`, which say the fingerprint. NEGATIVE CONTROL (by
/// scratch revert): the name's key marked host tier puts it in the journal,
/// the carry and `log`, and this fails; in-test, the content tier holds both
/// plants, so the absence below it is the tier's, not the writer's.
#[test]
fn a_topic_is_a_fingerprint_wherever_its_record_leaves_ram() {
    let (h, _store) = fabric_rig(73);
    let ctx = Arc::clone(&h.ctx);
    assert_eq!(
        ctx.fabric
            .topic_add("QZP6TOPIC1", "@QZP6SINCE1", &ctx.timeline),
        Ok(true)
    );
    assert!(ctx.fabric.topic_drop("QZP6TOPIC1", &ctx.timeline));
    // A broadcast on a topic (6F-6): the post's `to=say:<topic>`.
    let reply =
        crate::fabric::cmd_post(&ctx, "to=say:qzp6topic3 kind=note --wait=0 SENTINEL", None);
    assert!(queued(&reply, 1), "{reply}");
    let fp = format!("{:016x}", super::super::name_fp(Some("QZP6TOPIC1")));
    let say = format!("{:016x}", super::super::name_fp(Some("qzp6topic3")));
    let held = content(&h);
    for plant in [
        "topic_name=QZP6TOPIC1",
        "since=@QZP6SINCE1",
        "topic_name=qzp6topic3",
    ] {
        assert!(
            held.contains(plant),
            "the content tier holds {plant}: {held}"
        );
    }
    let out = below_content(&h);
    for what in ["log host", "journal", "carry"] {
        for token in [format!(" fp={fp}"), format!(" to=say:{say}")] {
            assert!(
                out.iter().any(|(w, l)| *w == what && l.contains(&token)),
                "{what} says the fingerprint ({token}): {out:?}"
            );
        }
    }
    assert!(
        out.iter()
            .any(|(w, l)| *w == "log wire" && l.contains(&format!(" fp={fp}"))),
        "the wire says the topic's fingerprint: {out:?}"
    );
    for (what, line) in &out {
        for planted in ["QZP6TOPIC1", "QZP6SINCE1", "qzp6topic3"] {
            assert!(!line.contains(planted), "{planted} in the {what}: {line}");
        }
    }
}

/// A HOLD'S REASON IS RAM ONLY (6F-6; ruling U12): the owner's planted
/// reason is the `hold` mark's at the content tier, beside the row's
/// `origin=` and `hold_origin=` below it, and in no journal frame, no carry
/// line, no bus body, no `brief` and no `log` line below `tier=content`.
/// NEGATIVE CONTROL (by scratch revert): `reason=` marked host tier puts it
/// in the journal and `log`, and this fails; in-test, the content tier
/// holds it, so its absence below is the tier's.
#[test]
fn a_hold_reason_is_never_journaled_carried_or_published() {
    let (h, store) = fabric_rig(74);
    let sid = h.ctx.self_id.as_str().to_string();
    for rest in ["on reason=QZP6REASON1", "off"] {
        let reply = crate::fabric::cmd_hold_waking(
            &store,
            &format!("{sid} {rest}"),
            crate::fabric::HoldIssuer::Owner,
            &|_| {},
        );
        assert!(reply.starts_with("OK hold="), "{reply}");
    }
    let held = content(&h);
    assert!(
        held.contains(" mark - hold state=on origin=local hold_origin=local reason=QZP6REASON1 "),
        "{held}"
    );
    let out = below_content(&h);
    assert!(
        out.iter().any(|(w, l)| *w == "journal"
            && l.contains(" mark - hold state=on origin=local hold_origin=local ")),
        "the journal holds the hold: {out:?}"
    );
    for (what, line) in &out {
        assert!(
            !line.contains("QZP6REASON1"),
            "the reason in the {what}: {line}"
        );
    }
}

/// A SESSION'S CLOSER AND A KEYED WRITE'S REPLY ARE CONTENT TIER (6F-7;
/// ruling U12): the raw actor that closed a session (a name no one minted,
/// so the close's `by=` says `-`, 3A's R1) is the session close's
/// `closer=`, and the first two words of a keyed write's reply the
/// `in-doubt` mark's `reply=`, each at the content tier beside the fields
/// below it, and in no journal frame, no carry line, no bus body, no
/// `brief` and no `log` line below `tier=content`. NEGATIVE CONTROL (by
/// scratch revert): `closer=` marked host tier puts it in the journal and
/// `log`, and this fails; in-test, the content tier holds both, so their
/// absence below is the tier's.
#[test]
fn a_closer_and_a_reply_never_leave_ram() {
    let (h, store) = fabric_rig(76);
    let key = crate::pty_idem::parse_key(&format!("{}:3:9", h.ctx.nonce.to_hex()), h.ctx.nonce)
        .expect("a key");
    let _ = crate::pty_idem::record_in_doubt(&h.ctx, "send", key, "QZP6REPLY2 failed\n");
    // The reply, while the session is open (and so carried).
    let open = below_content(&h);
    assert!(
        open.iter()
            .any(|(w, l)| *w == "carry" && l.contains(" producer=3 key_seq=9 ")),
        "the carry holds the in-doubt mark: {open:?}"
    );
    for (what, line) in &open {
        assert!(
            !line.contains("QZP6REPLY2"),
            "the reply in the {what}: {line}"
        );
    }
    let _ = store.write().unwrap().deregister_local_as(
        76,
        ExitReason::CtlClose,
        ExitActor::Sid("qzp6closer1".into()),
    );
    let held = content(&h);
    assert!(
        held.contains(" mark - in-doubt verb=send producer=3 key_seq=9 reply=QZP6REPLY2-failed "),
        "{held}"
    );
    // The close names no session for a sid no one minted (no `by=`, R1).
    let close = held
        .lines()
        .find(|l| l.contains(" close - "))
        .expect("the session close");
    assert!(
        close.contains(" out=ctl-close ")
            && close.contains(" closer=qzp6closer1 ")
            && !close.contains(" by="),
        "{close}"
    );
    // The closer, once the session closed: a closed session is carried no
    // more.
    let out = below_content_of(&h, false);
    assert!(
        out.iter()
            .any(|(w, l)| *w == "journal" && l.contains(" producer=3 key_seq=9 ")),
        "the journal holds the in-doubt mark: {out:?}"
    );
    assert!(
        out.iter().any(|(w, l)| *w == "journal"
            && l.contains(" close - ")
            && l.contains(" out=ctl-close")),
        "the journal holds the close: {out:?}"
    );
    for (what, line) in &out {
        assert!(
            !line.contains("qzp6closer1") && !line.contains("QZP6REPLY2"),
            "content in the {what}: {line}"
        );
    }
}

/// A POST'S RELAY HOPS AND ITS CALLER'S KEY ARE HOST TIER (6F-6): the
/// planted `via=` and `key=` are the `mail dir=out` mark's `via=` and
/// `idem=` (never `key=`, the config key's) in `log` at the host tier and
/// the journal, and in no bus body and no `log` line at the wire tier.
/// NEGATIVE CONTROLS (by scratch revert): the caller's key written to a
/// wire key reaches the bus, and this fails; `idem=` marked wire tier is
/// refused by the key table (a text key is host or content only), and the
/// mark then holds no `idem=`, and this fails.
#[test]
fn a_posts_idem_and_via_never_reach_a_bus_body() {
    let (h, _store) = fabric_rig(75);
    let reply = crate::fabric::cmd_post(
        &h.ctx,
        "to=@s-peer kind=ask dl=5000 via=n-qzp6via1 key=QZP6IDEM1 --wait=0 SENTINEL",
        None,
    );
    assert!(queued(&reply, 1), "{reply}");
    let out = below_content(&h);
    for (what, line) in out.iter().filter(|(w, _)| matches!(*w, "bus" | "log wire")) {
        assert!(
            line.contains(" dl=5000") || !line.contains(" dir=out"),
            "{line}"
        );
        for planted in ["QZP6IDEM1", "qzp6via1"] {
            assert!(!line.contains(planted), "{planted} in the {what}: {line}");
        }
    }
    for what in ["log host", "journal"] {
        assert!(
            out.iter().any(|(w, l)| *w == what
                && l.contains(" dl=5000 via=n-qzp6via1 idem=QZP6IDEM1 ")
                && !l.contains(" key=")),
            "the {what} holds them: {out:?}"
        );
    }
}

/// A REPEATED `inbox seen` MOVES NO MAIL COUNT (6F-6), through the verb:
/// two rows delivered, the first seen with a verdict, then seen again
/// twice; every seen is a `mail-seen` mark, the repeats `moved=0`, and the
/// fold's mail counts are the first seen's. NEGATIVE CONTROL (by scratch
/// revert): a fold that reads the unmoved marks closes the second row's
/// mail too (`aterm-slog`'s `a_repeated_inbox_seen_moves_no_mail_count`
/// fails beside this).
#[test]
fn a_repeated_inbox_seen_leaves_the_folds_mail_counts() {
    let (h, store) = fabric_rig(76);
    let sid = h.ctx.self_id.as_str().to_string();
    for (off, reply) in [(7, "OK 1\n"), (8, "OK 2\n")] {
        let line = format!("{sid} off={off} from=h-andrew kind=task trust=human text=SENTINEL");
        assert_eq!(
            crate::fabric::cmd_deliver_waking(&store, &line, &|_| {}),
            reply
        );
    }
    let counts = || {
        let g = h.ctx.timeline.lock().unwrap();
        let st = g.slog().log().state();
        (st.mail_open, st.totals.mail_in)
    };
    assert_eq!(counts(), (2, 2));
    for _ in 0..3 {
        assert_eq!(
            crate::fabric::cmd_inbox_seen(&h.ctx, "1 handled"),
            "OK seen=1\n"
        );
        assert_eq!(counts(), (1, 2), "{}", content(&h));
    }
    let lines = content(&h);
    assert_eq!(lines.matches(" mark - mail-seen ").count(), 3, "{lines}");
    assert_eq!(lines.matches(" moved=0 ").count(), 2, "{lines}");
}
