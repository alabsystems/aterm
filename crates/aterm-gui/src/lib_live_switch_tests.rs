// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{LiveSwitch, LoopGate, ParkEnd, ParkProbes, sleep_interval_watching_bump};
use std::time::{Duration, SystemTime};

fn scratch(label: &str) -> std::path::PathBuf {
    let dir =
        std::env::temp_dir().join(format!("aterm-live-switch-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("aterm.toml")
}

#[test]
fn a_config_edit_is_seen_by_the_next_read() {
    let path = scratch("edit");
    std::fs::write(&path, "[packages]\nenabled = true\n").unwrap();
    let mut switch = LiveSwitch::new(Some(path.clone()), LoopGate::On);
    assert_eq!(switch.poll(), LoopGate::On);
    std::fs::write(&path, "[packages]\nenabled = false\n").unwrap();
    assert_eq!(switch.poll(), LoopGate::Off, "turned off: seen at once");
    assert_eq!(
        switch.poll(),
        LoopGate::Off,
        "an unchanged file is not re-read"
    );
    std::fs::write(&path, "[packages]\nenabled = true\n").unwrap();
    assert_eq!(switch.poll(), LoopGate::On);
    std::fs::write(&path, "[packages]\nenabled = true\nauto_update = false\n").unwrap();
    assert_eq!(
        switch.poll(),
        LoopGate::Off,
        "the retired auto_update = false is the one switch, off"
    );
    std::fs::write(&path, "[packages]\nenabled = true\n").unwrap();
    assert_eq!(switch.poll(), LoopGate::On, "back on: resumed");
    std::fs::write(&path, "[packages]\nenabled = fal").unwrap();
    assert_eq!(switch.poll(), LoopGate::On);
    std::fs::write(&path, "[packages]\nenabled = false\n").unwrap();
    assert_eq!(switch.poll(), LoopGate::Off);
    std::fs::write(&path, "[packages\nenabled = true\n").unwrap();
    assert_eq!(
        switch.poll(),
        LoopGate::Off,
        "an unparsable file changes nothing"
    );
    std::fs::remove_file(&path).unwrap();
    assert_eq!(switch.poll(), LoopGate::On);
    let mut fixed = LiveSwitch::new(None, LoopGate::Off);
    assert_eq!(fixed.poll(), LoopGate::Off);
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
fn a_park_answers_the_switch_at_a_slice() {
    let path = scratch("park");
    std::fs::write(&path, "[packages]\nenabled = false\n").unwrap();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let switch_file = path.clone();
    std::thread::spawn(move || {
        super::set_park_slice_for_this_thread(Duration::from_millis(10));
        let end = sleep_interval_watching_bump(
            None,
            &mut (SystemTime::now() + Duration::from_secs(300)),
            Duration::from_secs(300),
            &mut super::BumpWatch::default(),
            ParkProbes::INDEX,
            None,
            &mut LiveSwitch::new(Some(switch_file), LoopGate::On),
        );
        let _ = done_tx.send(end);
    });
    let end = done_rx
        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .expect("the park heard the switch, not its deadline");
    assert_eq!(end, ParkEnd::SwitchedOff);
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
fn the_exclusion_rides_the_same_read() {
    let path = scratch("exclude");
    let mut switch = LiveSwitch::new(Some(path.clone()), LoopGate::On);
    assert!(switch.exclude().is_empty(), "nothing read yet");
    std::fs::write(&path, "[packages]\nexclude = [\"codex\"]\n").unwrap();
    assert_eq!(switch.poll(), LoopGate::On);
    assert_eq!(switch.exclude(), ["codex".to_string()]);
    std::fs::write(&path, "[packages]\nexclude = [\"claude\", \"codex\"]\n").unwrap();
    switch.poll();
    assert_eq!(
        switch.exclude(),
        ["claude".to_string(), "codex".to_string()]
    );
    std::fs::write(&path, "[packages\nexclude = []\n").unwrap();
    switch.poll();
    assert_eq!(switch.exclude().len(), 2, "a torn save keeps the list");
    std::fs::remove_file(&path).unwrap();
    switch.poll();
    assert!(switch.exclude().is_empty(), "gone: the defaults");
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}
