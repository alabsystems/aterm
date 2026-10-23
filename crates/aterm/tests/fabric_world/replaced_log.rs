// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **A REPLACED BUS LOG'S OFFSETS ARE ANOTHER LOG'S** (r2-01).

use crate::harness;

use harness::{FLEET, World, id_at_off, off_of, until};

fn replace_the_log(w: &mut World) {
    if let Some(mut h) = w.handle.take() {
        h.shutdown();
    }
    w.broker.take();
    std::fs::rename(&w.broker_log, w.tmp.join("bus.log.aside")).expect("move the log aside");
    let _ = std::fs::remove_file(&w.broker_sock);
    let broker = astream_broker::Broker::open_guarded(&w.broker_log, harness::SECRET.to_vec())
        .expect("a broker on a fresh log");
    w.handle = Some(broker.serve(&w.broker_sock).expect("re-serve"));
    w.broker = Some(broker);
}

#[test]
fn a_fresh_logs_records_at_the_old_logs_offsets_all_reach_the_session() {
    let mut w = World::boot("replaced-log-offsets", &[]);
    w.wait_ready();
    let (a, _b) = w.two_sessions();
    let node = w.node.clone();
    let forty = |src: &str, tag: &str, producer: u64| -> Vec<harness::Record> {
        (1..=40)
            .map(|i| {
                let subject = format!("/f/{FLEET}/in/{node}/{a}/{src}/note");
                (
                    producer,
                    i,
                    subject,
                    format!("v=1 t=1 text={tag}-{i}").into_bytes(),
                )
            })
            .collect()
    };
    let old = harness::publish_pipelined(&mut w.god(), &forty("h-old", "old", 6_300));
    until("the old log's forty rows", || {
        (w.inbox(&a).len() >= 40).then_some(())
    });

    replace_the_log(&mut w);
    until("the session's presence row on the new log", || {
        harness::presence_body(&w, &a)
    });
    let new = harness::publish_pipelined(&mut w.god(), &forty("h-new", "new", 6_301));
    let (old_offs, new_offs): (Vec<u64>, Vec<u64>) = (
        old.iter().map(|(o, _)| *o).collect(),
        new.iter().map(|(o, _)| *o).collect(),
    );
    assert!(
        new_offs.iter().any(|o| old_offs.contains(o)),
        "the two logs' offsets must overlap for this to test anything: {old_offs:?} {new_offs:?}"
    );
    until(
        "all eighty rows, the ones at the old log's offsets too",
        || (w.inbox(&a).len() >= 80).then_some(()),
    );
    let ev = w.ev();
    assert!(
        ev.iter().any(|e| e.starts_with("log-replaced head=")
            && e.contains(" log=")
            && e.contains(" was_log=")),
        "the new log is not told the node re-based: {ev:#?}"
    );
}

#[test]
fn a_receipt_owed_for_the_old_logs_row_is_retired_not_published_on_the_new_one() {
    let mut w = World::boot("replaced-log-receipt", &[]);
    w.wait_ready();
    let (a, b) = w.two_sessions();
    let ack_lane = format!("/f/{FLEET}/in/{}/{a}/{}/ack", w.node, w.node);
    let ask = w.verb(&format!("@{a} post to=@{b} kind=ask --wait=60000 decide"));
    assert!(ask.ok(), "ask: {} — log:\n{}", ask.header(), w.log_tail());
    let off = off_of(ask.header());
    let id = until("the ask to reach b", || id_at_off(&w, &b, off));

    w.stop_broker();
    let seen = w.verb(&format!("@{b} inbox seen {id} handled"));
    assert!(seen.ok(), "inbox seen: {}", seen.header());
    std::fs::rename(&w.broker_log, w.tmp.join("bus.log.aside")).expect("move the log aside");
    w.start_broker();

    let unacked = until("b's receipt to be retired on the new log", || {
        w.ev()
            .into_iter()
            .find(|e| e.starts_with(&format!("unacked re={off} ")))
    });
    assert!(
        unacked.ends_with(" verdict=handled reason=log-replaced"),
        "{unacked}"
    );
    let (rows, _) = w
        .read(|c| c.fetch(0, &ack_lane, 16))
        .expect("fetch a's ack lane");
    assert!(
        rows.is_empty(),
        "an ack naming the old log's offset reached the new one: {rows:?}"
    );
}
