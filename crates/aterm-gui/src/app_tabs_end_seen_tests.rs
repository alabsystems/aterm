// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for when an ended task's tab counts as seen.

use std::sync::Arc;

use aterm_agent::harness::acts::TaskFace;

use super::*;

const SEC: u64 = 1_000;

struct TestHost(crate::harness_host::HostHandle);

impl Drop for TestHost {
    fn drop(&mut self) {
        self.0.shutdown_and_join();
    }
}

fn desk() -> (App, String, TestHost) {
    let mut app = App::headless_for_test();
    let host = TestHost(crate::harness_host::HostHandle::for_supervised_test(
        Vec::new(),
        Arc::new(|_| crate::harness_host::BodyEnd::Stopped),
    ));
    app.harness = Some(host.0.clone());
    let ctx = app.pool.get(0).expect("session 0").ctx.clone();
    {
        let mut tl = ctx.timeline.lock().expect("timeline");
        tl.note_foreground_group(7);
        tl.set_program(7, Some("claude".into()));
    }
    ctx.meta
        .lock()
        .expect("meta")
        .set("autopilot", Some("land".into()));
    crate::session_timeline::claim_supervisor(
        &ctx,
        &crate::harness_host::holder(),
        None,
        None,
        crate::metrics::now_us(),
    )
    .expect("claimed");
    let id = app.next_session_id;
    app.push_stub_tab(WindowId(0), crate::stub_session(id));
    app.frontmost_window = Some(WindowId(0));
    app.windows
        .get_mut(&WindowId(0))
        .expect("window 0")
        .window_on_screen_for_test = true;
    (app, ctx.self_id.as_str().to_string(), host)
}

fn publish(app: &App, word: &'static str) -> bool {
    app.pool
        .get(0)
        .expect("session 0")
        .ctx
        .timeline
        .lock()
        .expect("timeline")
        .publish_agent(
            word,
            None,
            None,
            Some(aterm_phase::Program::Claude),
            crate::session_timeline::AgentStamp {
                generation: crate::control::ScreenGen { epoch: 1, seq: 1 },
                fp: 1,
            },
        )
}

fn end_after(app: &mut App, sid: &str, ms: u64) {
    tell(app, sid, Some(TaskFace::Open));
    assert!(publish(app, "busy"), "busy published");
    app.note_harness_phase(0);
    crate::turn_ledger::advance_for_test(ms);
    assert!(publish(app, "idle"), "idle published");
    app.note_harness_phase(0);
    tell(app, sid, Some(TaskFace::Landed));
}

fn tell(app: &mut App, sid: &str, face: Option<TaskFace>) {
    app.apply_harness_fact(sid, crate::HarnessFact::Task(face));
}

/// GOING TO A TAB IS A LOOK AT ITS END: a notice's click, the palette or `aterm ctl go` lands on
/// the tab whose work ended, and its `•` goes; an end after that is news again.
#[test]
fn going_to_a_session_is_a_look_at_its_end() {
    let (mut app, sid, host) = desk();
    end_after(&mut app, &sid, 45 * SEC);
    assert!(app.end_unseen(0), "PRECONDITION: the end stands unseen");
    app.take_status_rebuild();
    assert!(app.go_to_session(0), "its tab is shown");
    assert!(!app.end_unseen(0), "going there is a look");
    assert!(
        app.take_status_rebuild(),
        "the look owes the Agents surfaces a rebuild"
    );
    drop(host);
}
