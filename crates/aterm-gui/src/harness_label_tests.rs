// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tab Control labels this Mac without reading a file on the event loop.

use std::sync::mpsc;

use super::*;
use crate::silent_file::SilentFile;

/// `machines.toml` never answers a read (a home whose server stopped answering): building the
/// Harness page's Tab Control state returns at once, labelling this Mac by its raw name.
#[test]
fn tab_control_reads_no_names_file_on_the_event_loop() {
    let config = aterm_tempfile::tempdir().expect("scratch config");
    let file = config.path().join("machines.toml");
    let names = SilentFile::at(&file);
    let (built, state) = mpsc::channel();
    std::thread::spawn(move || {
        with_host_names_file(&file, || {
            let app = App::headless_for_test();
            let state = app.tab_control_state(crate::autopilot::HarnessPosture::default());
            let _ = built.send(state.computers.first().map(|c| c.name.clone()));
        });
    });
    let name = state.recv_timeout(aterm_spec::wait::MUST_HAPPEN);
    if name.is_err() {
        names.release();
    }
    assert_eq!(
        name.expect("the state was built within the hang detector's minute"),
        Some(format!("{} (this Mac)", raw_local_host_name_gui())),
    );
}

/// The negative control: the stats reading (a worker's) reads `[names]` from a regular
/// `machines.toml`, and Tab Control labels this Mac with what it read once the reading lands.
#[test]
fn tab_control_shows_the_label_the_reading_read() {
    let scratch = aterm_tempfile::tempdir().expect("scratch folders");
    let file = scratch.path().join("machines.toml");
    let raw = raw_local_host_name_gui();
    std::fs::write(&file, format!("[names]\n\"{raw}\" = \"Studio Desk\"\n")).expect("names");
    let (state, harness) = (scratch.path().join("state"), scratch.path().join("harness"));
    for dir in [&state, &harness] {
        std::fs::create_dir_all(dir).expect("a scratch folder");
    }
    let report = with_host_names_file(&file, || {
        read_harness_stats(
            Some(&state),
            Some(&harness),
            None,
            "label-test",
            unix_ms_now(),
            |_| Some(0),
            &FleetSlot::new(FleetAsk::default()),
        )
    })
    .expect("a reading");
    assert_eq!(report.local_label.as_deref(), Some("Studio Desk"));
    let mut app = App::headless_for_test();
    app.harness_screen.stats.report = Some(std::sync::Arc::new(report));
    let state = app.tab_control_state(crate::autopilot::HarnessPosture::default());
    assert_eq!(
        state.computers.first().map(|c| c.name.clone()),
        Some("Studio Desk (this Mac)".to_string()),
    );
}
