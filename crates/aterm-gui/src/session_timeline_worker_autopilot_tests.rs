// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{SessionMeta, keep_worker_autopilot, restore_carried_meta};
use aterm_agent::supervise::Autopilot;

fn carried(ctx: &crate::SessionCtx, state: &str) {
    let row = format!(
        "rev=3 state={state} from=s-0123456789abcdef0123 since=1789000000000 \
         result_bytes=0 attention=0"
    );
    crate::task_record::seed_carried(ctx, Some(&row));
    assert!(ctx.timeline.lock().unwrap().task().is_some(), "{row}");
}

#[test]
fn a_worker_with_no_word_keeps_land() {
    let word = |ctx: &crate::SessionCtx| ctx.meta.lock().unwrap().autopilot_word();
    let ctx = crate::stub_session(0).ctx.clone();
    carried(&ctx, "working");
    assert_eq!(word(&ctx), Some(Autopilot::Land), "the seed keeps it");
    restore_carried_meta(&ctx, &SessionMeta::default());
    assert_eq!(word(&ctx), None, "the graft put the carried none back");
    assert!(keep_worker_autopilot(&ctx));
    assert_eq!(word(&ctx), Some(Autopilot::Land));
    let cruise = SessionMeta {
        autopilot: Some("cruise".into()),
        ..SessionMeta::default()
    };
    restore_carried_meta(&ctx, &cruise);
    assert!(!keep_worker_autopilot(&ctx));
    assert_eq!(word(&ctx), Some(Autopilot::Cruise), "a carried word stands");
    let plain = crate::stub_session(1).ctx.clone();
    assert!(!keep_worker_autopilot(&plain));
    assert_eq!(word(&plain), None, "no task: the table's default");
    let exited = crate::stub_session(2).ctx.clone();
    carried(&exited, "exited");
    assert_eq!(word(&exited), None, "an exited worker");
}
