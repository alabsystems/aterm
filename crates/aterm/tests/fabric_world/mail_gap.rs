// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A9 END TO END: unread mail the broker's retention gave up is told to the
//! sessions of its node. The bridge is away while five mails reach one session
//! and the bulk stream moves the cut past them; a mail class of three mails'
//! bytes gives the two oldest up. Back on the same broker (its ledger, so its
//! count), the inbox group is told the range and the count, the bridge says
//! `link gap`, and every session the node serves names it in its `inbox`
//! header (`given_up=`, `given_up_count=`) and marks it in its log
//! (`mail-gap`). Every wait is a one-minute hang detector.

use std::time::Duration;

use astream_broker::{Retention, Rule};
use aterm_link::broker_rules::{EV_FILTER, MAIL_FILTER, MAIL_READER};

use super::harness::{FLEET, SECRET, World, broker_client, kv, open_broker, until};

/// Every mail's text and every `ev` record's body, in bytes.
const BODY: u64 = 4096;

/// The bulk stream cut to two of its records' bytes, unread mail carried below
/// the cut within three mails' bytes, everything else kept.
fn tiny_rules() -> Retention {
    Retention::new(
        vec![
            Rule::prefix(EV_FILTER)
                .expect("the ev filter")
                .max_bytes(5 * BODY / 2),
            Rule::hold(MAIL_FILTER)
                .expect("the mail filter")
                .max_bytes(7 * BODY / 2)
                .until_read(MAIL_READER)
                .expect("the inbox group template"),
        ],
        Rule::default_keeping_all(),
    )
    .expect("the tiny rules")
}

/// `@<sid> inbox`'s header once it names a give-up.
fn given_up_header(w: &World, sid: &str) -> String {
    until(&format!("{sid}'s inbox to name the given-up mail"), || {
        let r = w.verb(&format!("@{sid} inbox --peek --meta"));
        let header = r.header().to_string();
        header.contains(" given_up=").then_some(header)
    })
}

/// `@<sid>`'s `mail-gap` marks, once there is one.
fn gap_marks(w: &World, sid: &str) -> Vec<String> {
    until(&format!("{sid}'s mail-gap mark"), || {
        let r = w.verb(&format!("@{sid} log kinds=mail-gap"));
        assert!(r.ok(), "log kinds=mail-gap: {}", r.header());
        let rows = r.rows().to_vec();
        (!rows.is_empty()).then_some(rows)
    })
}

/// THE SESSION IS TOLD, BOTH WAYS: five mails to `a` while the bridge is
/// away, three `ev` records after them, and the tiny rules' tick gives up the
/// two oldest mails. Reattached, `a` is delivered the three held mails, and
/// both sessions (the node's inbox is one group, so the given-up mail's
/// recipient is not known) name `given_up=@<cursor>..@<third mail>` and
/// `given_up_count=2`, and mark it once: `mail-gap off=<cursor>
/// floor=<third mail> lost=2`, by the bridge. NC (run): a bridge whose reader
/// drops the notice leaves both unnamed and unmarked.
#[test]
fn unread_mail_the_broker_gave_up_is_named_by_inbox_and_marked_in_the_log() {
    let mut w = World::boot_flags_with("mgap", &["--log", "off"], &[]);
    w.wait_ready();
    let (a, b) = w.two_sessions();
    w.stop_broker();

    let broker = open_broker(&w.broker_log, &w.opts);
    broker.set_retention_interval(Duration::from_secs(3600));
    let side = w.tmp.join("side.sock");
    let mut side_handle = broker.serve(&side).expect("serve on the side socket");
    let mut god = broker_client(&side);
    let cap = astream_cap::mint(SECRET, &format!("/f/{FLEET}/>")).expect("mint the god cap");
    god.attach(&cap.filter, &cap.tag)
        .expect("attach the god cap");
    let mail = format!("/f/{FLEET}/in/{}/{a}/a-stranger/note", w.node);
    let text = format!("v=1 t=1 text={}", "m".repeat(BODY as usize));
    let offs: Vec<u64> = (1..=5)
        .map(|seq| {
            god.publish(9_220, seq, &mail, text.as_bytes())
                .expect("publish a mail")
                .0
        })
        .collect();
    let filler = vec![b'e'; BODY as usize];
    let stream = format!("/f/{FLEET}/pub/n-fill/s-fill/ev");
    for seq in 1..=3 {
        god.publish(9_221, seq, &stream, &filler)
            .expect("publish an ev record");
    }
    drop(god);
    broker
        .set_retention(tiny_rules())
        .expect("set the tiny rules");
    broker
        .retention_tick_at(broker.now_ms())
        .expect("the retention tick");
    let _ = std::fs::remove_file(&w.broker_sock);
    let handle = broker
        .serve(&w.broker_sock)
        .expect("serve the broker again");
    // Not the broker's last listener now, so this stops only the side socket's.
    side_handle.shutdown();
    w.broker = Some(broker);
    w.handle = Some(handle);
    until("the bridge to reattach", || {
        w.verb("status")
            .header()
            .contains(" fabric=connected ")
            .then_some(())
    });

    let header = given_up_header(&w, &a);
    let range = kv(&header, "given_up").expect("given_up=").to_string();
    let (from, to) = range
        .strip_prefix('@')
        .and_then(|r| r.split_once("..@"))
        .and_then(|(f, t)| Some((f.parse::<u64>().ok()?, t.parse::<u64>().ok()?)))
        .unwrap_or_else(|| panic!("given_up={range}"));
    assert_eq!(to, offs[2], "the oldest mail still held: {header}");
    assert!(from <= offs[0], "from the group's cursor: {header}");
    assert_eq!(kv(&header, "given_up_count"), Some("2"), "{header}");
    let other = given_up_header(&w, &b);
    assert_eq!(kv(&other, "given_up"), Some(range.as_str()), "{other}");
    assert_eq!(kv(&other, "given_up_count"), Some("2"), "{other}");

    until("the three held mails in a's inbox", || {
        (w.inbox(&a).len() == 3).then_some(())
    });
    for (row, off) in w.inbox(&a).iter().zip(&offs[2..]) {
        assert_eq!(kv(row, "off"), Some(off.to_string().as_str()), "{row}");
    }

    for sid in [&a, &b] {
        let marks = gap_marks(&w, sid);
        assert_eq!(marks.len(), 1, "marked once: {marks:?}");
        let mark = &marks[0];
        assert!(
            mark.contains(&format!(" mail-gap off={from} floor={to} lost=2 ")),
            "{mark}"
        );
        assert!(mark.contains(" by=bridge src=fabric conf=exact "), "{mark}");
    }
}
