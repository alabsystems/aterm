// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn render(value: u32) -> String {
    let mut buf = [0u8; 8];
    String::from_utf8(fmt_u32_hex(value, &mut buf).to_vec()).unwrap()
}

#[test]
fn hex_formatter_matches_std_for_many_values() {
    for v in 0u32..2000 {
        assert_eq!(render(v), format!("{v:08X}"), "mismatch at {v}");
    }
    for v in [
        0,
        1,
        0x8000_0003, // breakpoint
        0xC000_0005,
        0xC000_00FD,
        u32::MAX,
    ] {
        assert_eq!(render(v), format!("{v:08X}"), "mismatch at {v:#010X}");
    }
}

#[test]
fn hex64_formatter_matches_std() {
    for v in [0u64, 1, 0xDEAD_BEEF, 0x7FF6_1234_5678_ABCD, u64::MAX] {
        let mut buf = [0u8; 16];
        assert_eq!(
            String::from_utf8(fmt_u64_hex(v, &mut buf).to_vec()).unwrap(),
            format!("{v:016X}"),
            "mismatch at {v:#018X}"
        );
    }
}

#[test]
fn install_is_idempotent() {
    let _one_at_a_time = super::super::PROCESS_MARKER_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    install();
    install();
    assert!(ARMED.load(Ordering::SeqCst));
}

#[test]
fn a_held_marker_survives_the_sweep_and_goes_once_released() {
    let dir = std::env::temp_dir().join(format!("aterm-marker-lock-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let held = dir.join("crash-signal-1-1.log");
    let file = open_marker_file(&held).expect("marker opened");
    super::super::sweep_stale_markers(&dir);
    assert!(held.exists(), "a live marker survives a sibling's sweep");
    let refused = std::fs::remove_file(&held).expect_err("no delete while held");
    assert_eq!(refused.raw_os_error(), Some(32), "a sharing violation");
    let readable = std::fs::read(&held).expect("the crash scan can still read it");
    assert!(readable.is_empty());
    drop(file);
    super::super::sweep_stale_markers(&dir);
    assert!(!held.exists(), "released, it is an ordinary empty leftover");

    let unheld = dir.join("crash-signal-2-2.log");
    let default_open = std::fs::File::create(&unheld).expect("default open");
    super::super::sweep_stale_markers(&dir);
    assert!(
        !unheld.exists(),
        "control: the default share mode lets a sweep unlink a live marker"
    );
    drop(default_open);
    let _ = std::fs::remove_dir_all(&dir);
}
