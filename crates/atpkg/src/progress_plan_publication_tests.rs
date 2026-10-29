// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn layout(label: &str) -> crate::testdir::ScratchLayout {
    crate::testdir::layout(&format!("atpkg-plan-publication-{label}-"))
}

fn cap_writes(sink: &ProgressSink) {
    sink.inner.lock().unwrap().last_write = Some(Instant::now() + Duration::from_secs(86_400));
}

fn promised_sink(layout: &crate::store::Layout) -> ProgressSink {
    promise_pass(layout, std::process::id(), PASS_PROMISE_SECS);
    let sink = ProgressSink::create(&layout.progress_file(), "net").unwrap();
    let before = read_progress(layout).unwrap();
    assert!(before.programs.is_empty());
    assert!(before.queue.is_empty());
    assert!(snapshot_running(&before, unix_now()));
    assert!(pass_owed(layout, unix_now()));
    sink
}

#[test]
fn a_promised_plan_lands_before_its_promise_retires_despite_the_write_cap() {
    let layout = layout("capped");
    let sink = promised_sink(&layout);
    cap_writes(&sink);
    sink.plan_promised_pass(&layout, &[("trust".into(), 17), ("ay".into(), 23)]);

    let live = read_progress(&layout).expect("the actual pending-reader snapshot");
    assert_eq!(
        live.queue,
        ["trust", "ay"],
        "the promise cannot vanish while only the live empty start snapshot is readable"
    );
    assert_eq!(live.programs["trust"].phase, Phase::Queued);
    assert_eq!(live.programs["ay"].bytes_total, 23);
    assert_eq!(live.overall.programs_total, 2);
    assert_eq!(live.overall.bytes_total, 40);
    assert!(
        !pass_owed(&layout, unix_now()),
        "a readable plan takes over"
    );
}

#[test]
fn a_promised_plan_keeps_its_promise_if_publication_fails() {
    let layout = layout("failed");
    let sink = promised_sink(&layout);
    let promise = std::fs::read(layout.pass_owed_file()).unwrap();
    let empty = std::fs::read(layout.progress_file()).unwrap();
    let blocked = layout
        .progress_file()
        .with_file_name(format!("progress.json.tmp-{}", std::process::id()));
    std::fs::create_dir(&blocked).unwrap();
    cap_writes(&sink);
    sink.plan_promised_pass(&layout, &[("trust".into(), 17)]);

    assert!(
        pass_owed(&layout, unix_now()),
        "a plan held only in memory must not retire the pending reader's promise"
    );
    assert_eq!(std::fs::read(layout.pass_owed_file()).unwrap(), promise);
    assert_eq!(std::fs::read(layout.progress_file()).unwrap(), empty);
    assert!(read_progress(&layout).unwrap().programs.is_empty());
    assert_eq!(
        sink.snapshot().unwrap().programs["trust"].phase,
        Phase::Queued
    );
    assert!(
        blocked.is_dir(),
        "the failed publication leaves the obstacle alone"
    );
}
