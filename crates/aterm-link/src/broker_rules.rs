// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE DEFAULT BROKER'S RETENTION (session log Phase 6 step 6E-2; rulings
//! U23(a), U24; `docs/DESIGN-session-log-2026-10-15.md` §5.5): the one place
//! the rules every broker aterm opens retains its log by are written, so
//! `aterm link broker` and the world harness's in-process broker open alike
//! ([`retain`]), and the World default is the production default.
//!
//! astream's retention classes (`astream_broker::retention`, its R4): exactly
//! one PREFIX rule holds the cut, every other rule HOLDS what it still wants
//! below it in the held store (`<log>.held`), and each class keeps within its
//! own bytes, so broker RAM is at most the sum of the rules' bytes plus the
//! `Last` rows the subject caps bound. The four rules:
//!
//! - `/f/*/pub/*/*/ev`, the bulk stream (the session-log mirror and every
//!   node's and session's `ev` records): PREFIX, 7 days or 256 MiB, which is
//!   four nodes at §5.5's 64 MiB share each, the share each node's mirror
//!   holds itself to ([`crate::mirror::NODE_WINDOW_BYTES`], ruling U41).
//! - `/f/*/pub/*/*/presence`: HOLD, the last value only (each subject's `Last`
//!   row, which every compaction carries whatever the rules).
//! - `/f/*/in/>`, the mail lanes: HOLD within 64 MiB. A hold rule never moves
//!   the cut, so mail above it stays, read or not; below it, a mail the
//!   recipient node's inbox group (`/f/<F>/cur/<node>/node/inbox`,
//!   [`crate::subject::inbox_group`]) has not read is carried for up to 7
//!   days, and a read one goes with the cut. A person's lane
//!   (`/f/<F>/in/p/<person>/…`) names no node inbox, so its mail is carried
//!   for its 7 days alone: longer than its read needs, never shorter. A mail
//!   given up unread is LOGGED by astream, one line naming its subject and
//!   count, and a `Fetch` below it answers `ERR retained-from=` (`inbox get
//!   @<off>` says it); the inbox's group subscription is not told,
//!   since astream's tail reads on from the floor.
//! - Every other subject (the `say` topics, `ack` faces, halts, and the
//!   broker's own bindings, wills and group commits, which `<log>.state`
//!   carries): the DEFAULT rule, HOLD, 7 days or 32 MiB.
//!
//! The fleet segment is a `*`: one broker serves one fleet root, and the rules
//! need not name it. The rules are set PER OPEN (astream retains and
//! advertises only the rules set since its open), so every open calls
//! [`retain`], as `asb serve --retain-file` does at every start.

use std::io;
use std::time::Duration;

use astream_broker::{Retention, Rule};

/// The bulk stream every node's and session's `ev` records go to: the one
/// prefix rule.
pub const EV_FILTER: &str = "/f/*/pub/*/*/ev";
/// Every presence row, node and session: the last value only.
pub const PRESENCE_FILTER: &str = "/f/*/pub/*/*/presence";
/// Every mail lane.
pub const MAIL_FILTER: &str = "/f/*/in/>";
/// The group whose read releases a mail: its recipient node's inbox
/// (`/f/<F>/in/<node>/…` → `/f/<F>/cur/<node>/node/inbox`).
pub const MAIL_READER: &str = "/f/{2}/cur/{4}/node/inbox";

/// How long any record is kept: §5.5's 7 days.
pub const MAX_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);
/// The bulk stream's bytes: four nodes at their full window share.
pub const EV_BYTES: u64 = 256 * 1024 * 1024;
/// The mail lanes' bytes.
pub const MAIL_BYTES: u64 = 64 * 1024 * 1024;
/// Every other subject's bytes.
pub const DEFAULT_BYTES: u64 = 32 * 1024 * 1024;

/// The rules every broker aterm opens retains its log by (the module doc).
///
/// # Panics
///
/// Never: every filter and template here is a constant astream's grammar
/// takes, which `the_default_rules_are_four_and_astream_takes_them` holds.
#[must_use]
pub fn default_rules() -> Retention {
    rules().unwrap_or_else(|e| unreachable!("the default retention rules are refused: {e}"))
}

fn rules() -> io::Result<Retention> {
    Retention::new(
        vec![
            Rule::prefix(EV_FILTER)?
                .max_age(MAX_AGE)
                .max_bytes(EV_BYTES),
            Rule::hold(PRESENCE_FILTER)?.last_only(),
            Rule::hold(MAIL_FILTER)?
                .until_read(MAIL_READER)?
                .max_age(MAX_AGE)
                .max_bytes(MAIL_BYTES),
        ],
        Rule::default_keeping_all()
            .max_age(MAX_AGE)
            .max_bytes(DEFAULT_BYTES),
    )
}

/// Whether `rules` (a broker's, as its `Hello` advertised them) RETAIN this
/// node's `ev` faces: a prefix rule whose filter holds every
/// `/f/<fleet>/pub/<node>/<sid>/ev`, so the broker gives the bulk stream up
/// from its oldest end and says where (`oldest=`, `retained-from=`). The mirror
/// is on by default only over such a broker (step 6E-3) and keeps to its
/// window budget there ([`crate::mirror::NODE_WINDOW_BYTES`], ruling U41); a
/// hold rule or the default rule alone is no such broker.
#[must_use]
pub fn retains_ev(rules: &Retention, fleet: &str, node: &str) -> bool {
    let Ok(faces) = astream_wire::Filter::new(format!("/f/{fleet}/pub/{node}/*/ev")) else {
        return false;
    };
    rules.rules().iter().any(|rule| {
        rule.class() == astream_broker::Class::Prefix
            && rule
                .filter()
                .and_then(|f| astream_wire::Filter::new(f).ok())
                .is_some_and(|f| f.contains(&faces))
    })
}

/// The rules in a few words, for `aterm link broker`'s start line:
/// `7 days, ev 256 MiB, mail 64 MiB, other 32 MiB`.
#[must_use]
pub fn summary() -> String {
    const MIB: u64 = 1024 * 1024;
    const DAY: u64 = 24 * 60 * 60;
    format!(
        "{} days, ev {} MiB, mail {} MiB, other {} MiB",
        MAX_AGE.as_secs() / DAY,
        EV_BYTES / MIB,
        MAIL_BYTES / MIB,
        DEFAULT_BYTES / MIB
    )
}

/// Retain `broker`'s log by [`default_rules`], from this open on: the one call
/// `aterm link broker` and the world harness make right after the open, before
/// anything is served, so the first `Hello` already advertises them.
///
/// THE OPEN'S OWN WRITES GO FIRST. A broker that owns its log fires every will
/// the log still holds as it opens, through its writer, and astream's
/// `set_retention` refuses a batch being written outside the log lock rather
/// than wait for it (`a batch is being written outside the log lock`). So the
/// rules are set behind a flush through the writer
/// ([`astream_broker::Broker::flush_durable`]): it is answered only once every
/// op queued before it, those wills among them, has committed, and before the
/// broker serves nothing else writes. A flush already covering the head is
/// skipped, a writer round trip.
///
/// # Errors
///
/// astream's refusal: a poisoned log, a flush that failed, or a held store
/// that cannot be written.
pub fn retain(broker: &astream_broker::Broker) -> io::Result<()> {
    broker.flush_durable()?;
    broker.set_retention(default_rules())
}

#[cfg(test)]
mod tests {
    use super::*;
    use astream_broker::{Class, Keep};

    /// The rules as the module doc states them, in astream's own terms: one
    /// prefix rule over the bulk stream, presence by its last value, mail held
    /// until its node's inbox read it, and the default rule; every bound the
    /// doc names. NEGATIVE CONTROL, run: a mail rule without its reader
    /// template fails it.
    #[test]
    fn the_default_rules_are_four_and_astream_takes_them() {
        /// One rule as astream reads it back: filter, class, keep, age, bytes.
        type Row<'a> = (Option<&'a str>, Class, Keep, Option<Duration>, Option<u64>);
        let r = rules().expect("astream takes the default rules");
        let got: Vec<Row<'_>> = r
            .rules()
            .iter()
            .chain(std::iter::once(r.default_rule()))
            .map(|rule| {
                (
                    rule.filter(),
                    rule.class(),
                    rule.keep(),
                    rule.age(),
                    rule.bytes(),
                )
            })
            .collect();
        assert_eq!(
            got,
            vec![
                (
                    Some(EV_FILTER),
                    Class::Prefix,
                    Keep::All,
                    Some(MAX_AGE),
                    Some(EV_BYTES)
                ),
                (Some(PRESENCE_FILTER), Class::Hold, Keep::Last, None, None),
                (
                    Some(MAIL_FILTER),
                    Class::Hold,
                    Keep::All,
                    Some(MAX_AGE),
                    Some(MAIL_BYTES)
                ),
                (
                    None,
                    Class::Hold,
                    Keep::All,
                    Some(MAX_AGE),
                    Some(DEFAULT_BYTES)
                ),
            ]
        );
        assert_eq!(r.rules()[2].readers(), [MAIL_READER.to_string()]);
        assert_eq!(default_rules(), r, "one constructor, one answer");
        assert_eq!(summary(), "7 days, ev 256 MiB, mail 64 MiB, other 32 MiB");
    }

    /// WHAT EACH RULE NAMES, by the fabric's own subject builders: a session's
    /// and a node's `ev` face are the bulk stream's, presence the last-value
    /// rule's, a node's and a person's mail the mail rule's, and a `say`
    /// topic, an `ack` face and a halt the default rule's. The reader template
    /// names the node inbox group the bridge commits. A filter that missed a
    /// face (`/f/*/pub/*/ev`, say) fails here.
    #[test]
    fn each_fabric_subject_falls_under_the_rule_the_doc_names() {
        let r = default_rules();
        let rule_of = |subject: &str| -> Option<&str> {
            r.rules()
                .iter()
                .find(|rule| {
                    rule.filter().is_some_and(|f| {
                        astream_wire::Filter::new(f).is_ok_and(|f| f.matches_str(subject))
                    })
                })
                .and_then(Rule::filter)
        };
        let (f, n) = ("f1", "n-0123456789abcdef");
        for ev in [
            crate::subject::session_face(f, n, "s-a", "ev"),
            crate::subject::node_face(f, n, "ev"),
        ] {
            assert_eq!(rule_of(&ev), Some(EV_FILTER), "{ev}");
        }
        for presence in [
            crate::subject::session_face(f, n, "s-a", "presence"),
            crate::subject::node_face(f, n, "presence"),
        ] {
            assert_eq!(rule_of(&presence), Some(PRESENCE_FILTER), "{presence}");
        }
        for mail in [
            crate::subject::post_lane(f, n, "s-a", "n-fedcba9876543210", "task"),
            crate::subject::person_lane(f, "h-jsmith", n),
        ] {
            assert_eq!(rule_of(&mail), Some(MAIL_FILTER), "{mail}");
        }
        for other in [
            format!("/f/{f}/pub/{n}/s-a/say/build"),
            crate::subject::node_face(f, n, "ack"),
            crate::subject::person_halt(f, "h-jsmith"),
        ] {
            assert_eq!(rule_of(&other), None, "the default rule's: {other}");
        }
        // The template's group for a node's mail is the one the bridge commits.
        let mail = crate::subject::post_lane(f, n, "s-a", "n-fedcba9876543210", "task");
        let segs: Vec<&str> = mail.split('/').skip(1).collect();
        let group = MAIL_READER.replace("{2}", segs[1]).replace("{4}", segs[3]);
        assert_eq!(group, crate::subject::inbox_group(f, n));
    }

    /// THE RULES ARE SET PAST A BATCH IN FLIGHT ([`retain`]): a batch held in
    /// its write window (the log lock released, the batch not committed), where
    /// a broker's open puts the wills it fires, and `retain` called there. It
    /// queues its flush behind the batch (the writer's round trips say so),
    /// and once the batch goes on the rules are set and advertised. NEGATIVE
    /// CONTROL, run: a `retain` that sets them with no flush first is refused
    /// in the window.
    #[cfg(unix)]
    #[test]
    fn the_rules_are_set_past_a_batch_in_flight() {
        let dir = aterm_tempfile::Builder::new()
            .prefix("al-retain")
            .tempdir_in("/tmp")
            .expect("scratch");
        let sock = dir.path().join("b.sock");
        let broker = crate::test_broker(dir.path().join("b.log")).expect("a broker");
        let _served = broker.serve(&sock).expect("serve the broker");
        let pause = broker.pause_next_write();
        let path = sock.to_string_lossy().into_owned();
        std::thread::scope(|scope| {
            let publisher = scope.spawn(move || {
                let (mut c, _) =
                    crate::transport::connect(&path, aterm_spec::wait::MUST_HAPPEN).expect("dial");
                c.publish(7, 1, "/f/f/pub/n-a/s-a/ev", b"v=1 t=1").map(drop)
            });
            assert!(
                pause.wait_at_most(aterm_spec::wait::MUST_HAPPEN),
                "the batch reaches its write window"
            );
            let before = broker.writer_round_trips();
            let (tx, rx) = std::sync::mpsc::channel();
            let b = &broker;
            scope.spawn(move || {
                let _ = tx.send(retain(b));
            });
            // Retain is queued behind the batch, or it answered in the window.
            let started = std::time::Instant::now();
            let early = loop {
                if let Ok(answer) = rx.try_recv() {
                    break Some(answer);
                }
                if broker.writer_round_trips() > before {
                    break None;
                }
                assert!(
                    started.elapsed() < aterm_spec::wait::MUST_HAPPEN,
                    "retain neither queued nor answered"
                );
                std::thread::yield_now();
            };
            assert!(
                early.is_none(),
                "retain answered inside the write window: {early:?}"
            );
            pause.resume();
            let answer = rx
                .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
                .expect("retain answers once the batch goes on");
            assert!(answer.is_ok(), "{answer:?}");
            publisher
                .join()
                .expect("the publisher")
                .expect("its publish lands");
        });
        assert_eq!(broker.retention(), Some(default_rules()));
    }

    /// WHICH RULES RETAIN A NODE'S `ev` FACES ([`retains_ev`]): the default
    /// rules do, for any fleet and node, and so does a prefix rule over a
    /// wider filter; a prefix rule over another face does not, nor a hold
    /// rule over the `ev` faces (held, not given up from the oldest end).
    /// NEGATIVE CONTROL, run: a judge that takes any rule over the faces fails
    /// the hold case.
    #[test]
    fn the_default_rules_retain_a_nodes_ev_faces_and_a_hold_rule_does_not() {
        let n = "n-0123456789abcdef";
        let with = |rules: Vec<Rule>| {
            Retention::new(rules, Rule::default_keeping_all()).expect("astream takes them")
        };
        assert!(retains_ev(&default_rules(), "f", n));
        assert!(retains_ev(&default_rules(), "lab", "n-fedcba9876543210"));
        assert!(retains_ev(
            &with(vec![Rule::prefix("/f/>").unwrap()]),
            "f",
            n
        ));
        let other = with(vec![Rule::prefix(PRESENCE_FILTER).unwrap()]);
        assert!(!retains_ev(&other, "f", n), "another face's prefix rule");
        let held = with(vec![
            Rule::prefix(MAIL_FILTER).unwrap(),
            Rule::hold(EV_FILTER).unwrap(),
        ]);
        assert!(!retains_ev(&held, "f", n), "a hold rule over the faces");
    }
}
