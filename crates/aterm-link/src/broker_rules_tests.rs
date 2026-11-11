// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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

/// THE MESH RULES ARE THE DEFAULT RULES, THE MESH'S MAIL HELD FOR THE CARRY TOO
/// ([`mesh_rules`], §11 C3d): every rule [`default_rules`] has, and ahead of the mail rule
/// one of the fleet `alab`'s mail lanes that names the person carry's group beside the push's
/// and forgets a lane retention gave up whole, for every subject the carry follows; another
/// fleet's mail falls under the mail rule still, which forgets nothing.
#[test]
fn the_mesh_rules_hold_the_meshs_mail_for_the_carry_too() {
    let (mesh, plain) = (mesh_rules(), default_rules());
    let mut without: Vec<Rule> = mesh.rules().to_vec();
    let carried = without.remove(2);
    assert_eq!(
        without,
        plain.rules(),
        "every other rule as the default rules have it"
    );
    assert_eq!(mesh.default_rule(), plain.default_rule());
    assert_eq!(
        (
            carried.filter(),
            carried.class(),
            carried.keep(),
            carried.age(),
            carried.bytes()
        ),
        (
            Some(MESH_MAIL_FILTER),
            Class::Hold,
            Keep::All,
            Some(MAX_AGE),
            Some(MAIL_BYTES)
        )
    );
    assert_eq!(
        carried.readers(),
        [MAIL_READER.to_string(), CARRY_READER.to_string()]
    );
    assert!(carried.forgets(), "the mesh rule forgets");
    assert!(
        plain.rules().iter().all(|rule| !rule.forgets()),
        "no default rule forgets"
    );
    assert_eq!(CARRY_READER, crate::federate::carry_group());
    assert_eq!(
        MESH_MAIL_FILTER,
        format!("/f/{}/in/>", crate::federate::FLEET)
    );
    let rule_of = |subject: &str| -> Option<&str> {
        mesh.rules()
            .iter()
            .find(|rule| {
                rule.filter().is_some_and(|f| {
                    astream_wire::Filter::new(f).is_ok_and(|f| f.matches_str(subject))
                })
            })
            .and_then(Rule::filter)
    };
    let n = "n-0123456789abcdef";
    let fleet = crate::federate::FLEET;
    let follows = astream_wire::Filter::new(crate::federate::posts()).expect("a filter");
    for mail in [
        crate::subject::post_lane(fleet, n, "s-q", "h-andrew", "ask"),
        crate::subject::post_lane(fleet, n, "s-q", "n-fedcba9876543210", "note"),
        format!(
            "{}/answer",
            crate::subject::person_lane(fleet, "h-andrew", n)
        ),
    ] {
        assert!(follows.matches_str(&mail), "the carry follows {mail}");
        assert_eq!(rule_of(&mail), Some(MESH_MAIL_FILTER), "{mail}");
    }
    let elsewhere = crate::subject::post_lane("f1", n, "s-a", "n-fedcba9876543210", "note");
    assert_eq!(rule_of(&elsewhere), Some(MAIL_FILTER), "{elsewhere}");
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
        .suffix(".noindex")
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
            aterm_uds::deadline::poll_yield();
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

/// THE START LINE NAMES THE LIMIT THE BROKER SERVES ([`rate_word`]): aterm's default by its
/// summary, another by its numbers, none as such.
#[test]
fn the_start_line_names_the_limit_the_broker_serves() {
    assert_eq!(
        rate_word(Some(rate_limit())),
        format!("rate-limited by aterm's default ({})", rate_summary())
    );
    assert_eq!(
        rate_word(astream_broker::RateLimit::records(10, 3)),
        "rate-limited at 10 records/s (burst 3) per producer"
    );
    assert_eq!(
        rate_word(astream_broker::RateLimit::records(10, 3).and_then(|l| l.with_bytes(5, 7))),
        "rate-limited at 10 records/s (burst 3), 5 bytes/s (burst 7) per producer"
    );
    assert_eq!(rate_word(None), "unlimited");
}
