// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// `[swarm] enabled = false` starts no Commons runtime, and a person turning it on starts
/// one; with the switch on, it starts at once.
#[test]
fn swarm_off_starts_no_commons_runtime_until_it_is_on() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let home = scratch.path();
    let config =
        aterm_types::dirs::resolve_aterm_config_path(None, None, Some(home.as_os_str())).unwrap();
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    aterm_uds::atomic_file::write(&config, b"[swarm]\nenabled = false\n", 0o600).unwrap();
    for enabled in [false, true] {
        std::fs::write(&config, format!("[swarm]\nenabled = {enabled}\n")).unwrap();
        let attempts = Arc::new(AtomicUsize::new(0));
        let tried = Arc::clone(&attempts);
        let mut startup =
            super::Startup::new(home, move || tried.fetch_add(1, Ordering::SeqCst) > 0);
        let now = std::time::Instant::now();
        let due = startup.maintain_at(now).unwrap();
        assert_eq!(attempts.load(Ordering::SeqCst), usize::from(enabled));
        let due = if !enabled {
            assert_eq!(due, now + super::SHARING_LOOK);
            std::fs::write(&config, "[swarm]\nenabled = true\n").unwrap();
            startup.maintain_at(due).unwrap()
        } else {
            due
        };
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        startup.maintain_at(due - std::time::Duration::from_nanos(1));
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        // A failed constructor never gives a later retry permission to bypass sharing.
        std::fs::write(&config, "[swarm]\nenabled = false\n").unwrap();
        let held = startup.maintain_at(due).unwrap();
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        std::fs::write(&config, "[swarm]\nenabled = true\n").unwrap();
        assert_eq!(startup.maintain_at(held), None);
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        assert_eq!(startup.maintain_at(held + super::SHARING_LOOK), None);
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }
}
