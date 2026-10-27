// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Default broker retention rules ([`retain`](crate::broker_rules::retain), [`crate::mirror::NODE_WINDOW_BYTES`],
//! [`crate::subject::inbox_group`]).
//!
//! The mail rule ([`MAIL_FILTER`]) carries unread mail below the cut until [`MAIL_READER`] reads
//! it, for [`MAX_AGE`] within [`MAIL_BYTES`]; read mail goes with the cut. Mail it gives up unread
//! is logged by astream (subject and count), answers a `Fetch` below it `ERR retained-from=`, and
//! is TOLD to the node's inbox group subscription (astream's gap notice, R4's addendum A9): the
//! bridge says `link gap from=<a> to=<b> [records=<n>]`, and every session the node serves names
//! it in `inbox` (`given_up=`) and marks it in its log (`mail-gap`). Read mail the cut took before
//! its session handled it is told to that session when the bridge's own read of its lane meets the
//! floor (its refill from `seen`, its deadline sweep): `link gap … sid=<sid>`.

use std::io;
use std::time::Duration;

use astream_broker::{Retention, Rule};

/// Subject pattern for the bulk `ev` stream.
pub const EV_FILTER: &str = "/f/*/pub/*/*/ev";
/// Every presence row, node and session.
pub const PRESENCE_FILTER: &str = "/f/*/pub/*/*/presence";
/// Every mail lane.
pub const MAIL_FILTER: &str = "/f/*/in/>";
/// The group whose read releases a mail.
pub const MAIL_READER: &str = "/f/{2}/cur/{4}/node/inbox";

/// How long any record is kept.
pub const MAX_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);
/// The bulk stream's bytes.
pub const EV_BYTES: u64 = 256 * 1024 * 1024;
/// The mail lanes' bytes.
pub const MAIL_BYTES: u64 = 64 * 1024 * 1024;
/// Every other subject's bytes.
pub const DEFAULT_BYTES: u64 = 32 * 1024 * 1024;

/// The rules every broker aterm opens retains its log by.
///
/// # Panics
/// Never: every filter and template here is a constant astream's grammar takes.
#[must_use]
pub fn default_rules() -> Retention {
    rules().unwrap_or_else(|e| unreachable!("the default retention rules are refused: {e}"))
}

/// [`default_rules`] with the bulk stream's, the mail lanes' and every other subject's bytes
/// scaled to `ev`, `mail` and `default`: the same filters, classes, reader and ages, so a test
/// reaches the bounds the tick cuts at with a few records.
///
/// # Panics
/// Never: astream checks the rules' filters, classes and readers, the same constants as
/// [`default_rules`]', and takes any byte bound.
#[cfg(any(test, debug_assertions))]
#[must_use]
pub fn rules_scaled(ev: u64, mail: u64, default: u64) -> Retention {
    rules_with(ev, mail, default)
        .unwrap_or_else(|e| unreachable!("the scaled retention rules are refused: {e}"))
}

fn rules() -> io::Result<Retention> {
    rules_with(EV_BYTES, MAIL_BYTES, DEFAULT_BYTES)
}

fn rules_with(ev: u64, mail: u64, default: u64) -> io::Result<Retention> {
    Retention::new(
        vec![
            Rule::prefix(EV_FILTER)?.max_age(MAX_AGE).max_bytes(ev),
            Rule::hold(PRESENCE_FILTER)?.last_only(),
            Rule::hold(MAIL_FILTER)?
                .until_read(MAIL_READER)?
                .max_age(MAX_AGE)
                .max_bytes(mail),
        ],
        Rule::default_keeping_all()
            .max_age(MAX_AGE)
            .max_bytes(default),
    )
}

/// Whether `rules` (a broker's, as its `Hello` advertised them) retain this node's `ev` faces.
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

/// The rules in a few words, for `aterm link broker`'s start line.
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

/// Retain `broker`'s log by [`default_rules`], from this open: called at once after the open,
/// with no flush first. astream sets the rules between the writer's batches, so they wait out
/// the batch the open's fired wills are in rather than meeting it in its write window.
///
/// # Errors
/// Astream's refusal: a poisoned log, or a held store that cannot be written.
pub fn retain(broker: &astream_broker::Broker) -> io::Result<()> {
    broker.set_retention(default_rules())
}

#[cfg(test)]
mod tests {
    use super::*;
    use astream_broker::{Class, Keep};

    /// The default rules in astream's own terms: one prefix rule over the bulk
    /// stream, presence by its last value, mail held until its node's inbox
    /// read it, and the default rule; every bound as the constants name it.
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
    /// topic, an `ack` face and a halt the default rule's.
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
        let mail = crate::subject::post_lane(f, n, "s-a", "n-fedcba9876543210", "task");
        let segs: Vec<&str> = mail.split('/').skip(1).collect();
        let group = MAIL_READER.replace("{2}", segs[1]).replace("{4}", segs[3]);
        assert_eq!(group, crate::subject::inbox_group(f, n));
    }

    /// THE RULES ARE SET PAST A BATCH IN FLIGHT ([`retain`]): a batch held in
    /// its write window (the log lock released, the batch not committed), where
    /// a broker's open puts the wills it fires, and `retain` called there. It
    /// waits for the batch (astream counts it, `retention_waits` 0 → 1) and does
    /// not answer inside it; resumed, the batch commits and the rules are set.
    /// Against an astream whose `set_retention` meets the batch instead, `retain`
    /// answers inside the window, refused "a batch is being written outside the
    /// log lock", and the first assertion after the wait fails.
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
            assert_eq!(broker.stats().retention_waits, 0, "PRECONDITION");
            let (tx, rx) = std::sync::mpsc::channel();
            let b = &broker;
            scope.spawn(move || {
                let _ = tx.send(retain(b));
            });
            let started = std::time::Instant::now();
            let early = loop {
                if let Ok(answer) = rx.try_recv() {
                    break Some(answer);
                }
                if broker.stats().retention_waits == 1 {
                    break None;
                }
                assert!(
                    started.elapsed() < aterm_spec::wait::MUST_HAPPEN,
                    "retain neither waited nor answered"
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
            assert_eq!(broker.stats().retention_waits, 1, "one wait, the batch's");
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
