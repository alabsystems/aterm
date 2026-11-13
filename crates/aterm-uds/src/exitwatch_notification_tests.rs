// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
#[test]
fn notification_observes_the_completed_stamp_before_it_can_return() {
    let mut child = crate::spawnfd::Command::new("/bin/sleep")
        .arg("600")
        .spawn()
        .unwrap();
    let (sent, seen) = std::sync::mpsc::sync_channel(1);
    let (release, wait) = std::sync::mpsc::sync_channel(1);
    let watch = ExitMoment::watch_then(child.id(), std::time::Instant::now, move || {
        sent.send(()).unwrap();
        let _ = wait.recv_timeout(std::time::Duration::from_secs(60));
    });
    let watch_error = std::io::Error::last_os_error();
    let Some(watch) = watch else {
        child.kill().unwrap();
        child.wait().unwrap();
        if cfg!(target_os = "linux")
            && watch_error.raw_os_error() == Some(38)
            && let Some(translator) = crate::process::translator()
        {
            println!(
                "NOT RUN under {translator}: exitwatch::notification_tests::notification_observes_the_completed_stamp_before_it_can_return: pidfd_open returned ENOSYS"
            );
            return;
        }
        #[cfg(any(target_vendor = "apple", target_os = "linux"))]
        panic!("native exit watcher unavailable");
        #[cfg(not(any(target_vendor = "apple", target_os = "linux")))]
        return;
    };
    let started = std::time::Instant::now();
    child.kill().unwrap();
    child.wait().unwrap();
    seen.recv_timeout(std::time::Duration::from_secs(60))
        .unwrap();
    // Callback is still blocked: moving stamp publication after it is a
    // deterministic failure, rather than a timing-sensitive race test.
    assert!(watch.get().is_some_and(|stamp| stamp >= started));
    release.send(()).unwrap();
}
