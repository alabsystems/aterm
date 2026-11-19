// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::bus::{Broker, Escalation, Stop, Tier};
use super::conform::{add_text, elsewhere};
use super::{lock, log_of, sender, LogReads, Rng};
use crate::body::Body;

fn from(f: &str) -> Body {
    Body {
        from: Some(f.to_string()),
        ..Body::default()
    }
}

/// The sim names the sender of a node-written record as the bridge does (`fabric::render_from`):
/// the session its `from=` names, in either tab-ID form; a malformed ID of either form, or a
/// `from=` on a session's own lane, leaves the sender the subject's `src`.
#[test]
fn a_node_written_record_is_sent_by_the_session_its_from_names_in_either_form() {
    for sid in ["s-0123456789abcdef0123", "tqn5r6e0148"] {
        assert_eq!(sender("n-a", &from(sid)), sid);
        assert_eq!(
            sender("s-0123456789abcdef0123", &from(sid)),
            "s-0123456789abcdef0123"
        );
    }
    for malformed in ["s-0123456789ABCDEF0123", "tqn5r6e014i", "tqn5r6e01", "s-"] {
        assert_eq!(sender("n-a", &from(malformed)), "n-a", "{malformed}");
    }
    assert_eq!(sender("n-a", &Body::default()), "n-a");
}

/// What the laws read of the model broker's log ([`log_of`]) is the log as it stands, record for
/// record, and a fold of it ([`super::Fold`]) is that of the records as they stand: through
/// appends, a read of a log that did not change, a log set aside, read while it is shorter than
/// at the read before (`log_of`'s shrink), and a power loss that keeps a part of the log, read
/// only once other records landed at the offsets it freed (`log_of`'s overwrite in place). The
/// tap's index of texts
/// ([`add_text`]) answers [`elsewhere`] as a scan of every offset does.
#[test]
fn what_the_laws_read_of_the_log_is_the_log_through_appends_and_cuts() {
    let broker = Broker::new(Tier::Relaxed, Escalation::aterm());
    let reads = LogReads::default();
    let mut seq = 0;
    let mut publish = |names: &[&str]| {
        for name in names {
            seq += 1;
            broker
                .publish_as(1, seq, &format!("/t/{name}"), name.as_bytes())
                .expect("the broker takes the record");
        }
    };
    let check = |stage: &str| {
        let log: Vec<String> = lock(&broker.state)
            .log
            .iter()
            .map(|r| r.subject.clone())
            .collect();
        let seen = log_of(&broker, &reads);
        let read: Vec<&str> = seen.iter().map(|r| r.subject.as_str()).collect();
        assert_eq!(read, log, "{stage}: the log as the laws read it");
        let folded = reads.inbox_records.of(&seen, |out, _, r| {
            out.push(Some((String::new(), r.subject.clone())));
        });
        let folded: Vec<&str> = folded.iter().flatten().map(|(_, t)| t.as_str()).collect();
        assert_eq!(folded, log, "{stage}: the fold of the log");
        let texts = reads.inbox_texts.of(&seen, |texts, off, r| {
            add_text(texts, off, Some(&(String::new(), r.subject.clone())));
        });
        for text in &log {
            for at in 0..=log.len() {
                let scan = log.iter().enumerate().any(|(o, t)| o != at && t == text);
                assert_eq!(
                    elsewhere(&texts, text, at),
                    scan,
                    "{stage}: whether a record other than @{at} holds {text}"
                );
            }
        }
    };
    check("empty");
    publish(&["a", "b", "a"]);
    check("appended");
    publish(&["c"]);
    check("appended again");
    check("unchanged");
    broker.stop_keeping_all(Stop::Clean);
    broker.retire_log().expect("a stopped log is set aside");
    broker.start();
    check("a log set aside, nothing landed since");
    publish(&["d", "c", "e", "d"]);
    check("other records at the offsets a log set aside freed");
    let cut = broker.stop(Stop::PowerLoss, &mut Rng::new(7));
    assert!(
        cut.kept > 0 && cut.kept < cut.before.len() as u64,
        "the power loss keeps a part of the log: {} of {}",
        cut.kept,
        cut.before.len()
    );
    broker.start();
    publish(&["f", "a", "g"]);
    check("after a power loss");
}
