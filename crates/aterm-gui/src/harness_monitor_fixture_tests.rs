// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const EXAMPLE: &str = include_str!("../fixtures/monitor.json");

#[test]
fn private_monitor_fixture_uses_census_and_cached_facts_without_reading_outside_home() {
    let root = aterm_tempfile::tempdir().expect("owned fixture world");
    let home = root.path().join("home");
    std::fs::create_dir(&home).expect("scratch HOME");
    let fixture = home.join("monitor.json");
    std::fs::write(&fixture, EXAMPLE).expect("fixture");
    let (computers, tabs) = read_in(&fixture, &home).expect("offline projection");
    assert_eq!(computers.len(), 4);
    let enriched = computers.iter().find(|row| row.name == "m100").unwrap();
    assert_eq!(enriched.os_version.as_deref(), Some("macOS 27.0"));
    assert_eq!(enriched.instances, Some(2));
    assert_eq!(enriched.tabs, Some(2));
    assert_eq!(enriched.ai_clis, Some(2));
    assert_eq!(enriched.active_ai, Some(1));
    assert!(
        enriched
            .vitals
            .starts_with("CPU 18c \u{b7} load 2.50/3.00/4.00"),
        "{}",
        enriched.vitals
    );
    assert!(enriched.vitals.contains("(75% \u{b7} 256 GiB free)"));
    assert_eq!(
        tabs.len(),
        2,
        "distinct sessions remain while the repeated alias observation shares one row"
    );
    assert_eq!(tabs[0].computer, "m100");
    let unknown = computers.iter().find(|row| row.name == "m101").unwrap();
    assert_eq!((unknown.tabs, unknown.ai_clis), (None, None));
    let zero = computers.iter().find(|row| row.name == "m102").unwrap();
    assert_eq!((zero.tabs, zero.ai_clis), (Some(0), Some(0)));
    let outside = root.path().join("outside.json");
    std::fs::write(&outside, EXAMPLE).expect("outside fixture");
    assert!(read_in(&outside, &home).is_none());
    #[cfg(unix)]
    {
        let link = home.join("outside.json");
        std::os::unix::fs::symlink(&outside, &link).expect("escaping link");
        assert!(read_in(&link, &home).is_none());
    }
    assert!(project("{}").is_none());
}
