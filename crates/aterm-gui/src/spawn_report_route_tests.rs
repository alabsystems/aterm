// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{ReportRoute, new_live_terminal, spawn_reply_writer};
use aterm_session::sink::SinkWriter;
use aterm_spec::wait::{MUST_HAPPEN, within};
use std::sync::{Arc, Mutex};
use std::time::Duration;

fn sink() -> Arc<SinkWriter> {
    Arc::new(SinkWriter::new(0x7fff_0f0f))
}

#[test]
fn a_report_goes_the_input_way() {
    let term = Arc::new(Mutex::new(new_live_terminal(24, 80, None, None)));
    let modes = Arc::clone(crate::term_lock(&term).mode_mirror());
    let sink = sink();
    let before = sink.driven_input();
    let (lane, _join) =
        spawn_reply_writer(sink.clone(), ReportRoute::of(&term, &modes)).expect("the reply writer");
    assert!(lane.submit_report(Arc::from(&b"\x1b[48;30;100;600;900t"[..])));
    let driven = within(MUST_HAPPEN, Duration::from_millis(1), || {
        (sink.driven_input() != before).then_some(())
    });
    assert!(driven.is_some(), "the report was never input");
}

#[test]
fn a_reply_and_a_routeless_report_are_not_input() {
    for reply in [true, false] {
        let sink = sink();
        let (epoch, driven) = (sink.input_epoch(), sink.driven_input());
        let (lane, _join) =
            spawn_reply_writer(sink.clone(), ReportRoute::default()).expect("the reply writer");
        let bytes = Arc::from(&b"\x1b[1;1R"[..]);
        assert!(if reply {
            lane.submit(bytes)
        } else {
            lane.submit_report(bytes)
        });
        let written = within(MUST_HAPPEN, Duration::from_millis(1), || {
            (sink.input_epoch() != epoch).then_some(())
        });
        assert!(written.is_some(), "the writer wrote it");
        assert_eq!(sink.driven_input(), driven, "not input anyone drove");
    }
}
