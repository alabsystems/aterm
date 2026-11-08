// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Local custody and writer admission when the caller names no deadline.

use std::time::{Duration, Instant};

const HANG: Duration = Duration::from_secs(60);

/// When an ordinary local admission stops waiting.
pub(crate) fn deadline() -> Instant {
    deadline_at(Instant::now())
}

pub(crate) fn deadline_at(started: Instant) -> Instant {
    started + HANG
}

#[cfg(all(test, unix))]
pub(crate) fn with_delayed_custody<R>(
    read: impl FnOnce(&aterm_secrets::client::Paths, Instant) -> R,
) -> R {
    use std::os::unix::fs::MetadataExt as _;
    let lab = crate::table::tests::Lab::new(&[aterm_secrets::commons::roster::Role::Writer]);
    lab.bootstrap(0);
    let paths = &lab.paths[0];
    let held = aterm_uds::private_file::PrivateLock::take_with(
        &paths.dir,
        "commons.lock",
        Duration::ZERO,
        "private custody held",
        || {},
    )
    .unwrap();
    let named = std::fs::metadata(paths.dir.join("commons.lock")).unwrap();
    let identity = (named.dev(), named.ino());
    let mut held = Some(held);
    aterm_uds::file_lock::with_lock_wait_observer(
        move |file| {
            let actual = file.metadata().unwrap();
            assert_eq!((actual.dev(), actual.ino()), identity);
            if let Some(held) = held.take() {
                drop(held);
            }
        },
        || read(paths, Instant::now() - Duration::from_secs(3)),
    )
}
