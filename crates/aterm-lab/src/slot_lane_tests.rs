// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{duration_of, slot_files, slot_holders};
use std::time::Duration;

/// The watch's holder scan ([`slot_holders`]) names each slot file a process holds open
/// with that process. NEGATIVE CONTROLS: a slot file nobody holds names no holder, and one
/// removed after the listing is held by nobody rather than failing the watch.
#[test]
fn the_watch_names_each_held_slot_file_with_its_holder() {
    let dir = crate::TestScratch::new("slots");
    for n in 0..3 {
        std::fs::write(dir.join(format!("slot-{n}")), b"").unwrap();
    }
    let mut holder = aterm_uds::spawnfd::command("/bin/sleep")
        .arg("600")
        .stdin(std::fs::File::open(dir.join("slot-0")).unwrap())
        .spawn()
        .expect("spawn a holder");
    let files = slot_files(&dir);
    std::fs::remove_file(dir.join("slot-2")).unwrap();
    let scan = slot_holders(&files);
    let _ = holder.kill();
    let _ = holder.wait();
    assert_eq!(files.len(), 3, "the listing");
    let scan = scan.expect("the scan");
    assert_eq!(scan.held, [(dir.join("slot-0"), holder.id())]);
}

#[test]
fn watch_duration_requires_a_positive_bounded_number_and_an_ascii_unit() {
    for (text, seconds) in [("1s", 1), ("30s", 30), ("20m", 1200), ("24h", 86400)] {
        assert_eq!(
            duration_of(text),
            Some(Duration::from_secs(seconds)),
            "{text}"
        );
    }
    for text in [
        "",
        "s",
        "0s",
        "1",
        "-1s",
        "1é",
        "é",
        "1😀",
        "1秒",
        "18446744073709551616s",
        "18446744073709551615m",
        "18446744073709551615h",
    ] {
        assert_eq!(duration_of(text), None, "{text}");
    }
    assert_eq!(
        duration_of("18446744073709551615s"),
        Some(Duration::from_secs(u64::MAX))
    );
}
