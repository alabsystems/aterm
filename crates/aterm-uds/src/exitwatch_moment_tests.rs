// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{ExitMoment, exit_moment_thread};

use crate::spawnfd::Command;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Each stamp taken, with the name of the thread that took it.
static TAKEN: Mutex<Vec<(Option<String>, Instant)>> = Mutex::new(Vec::new());

fn stamp() -> Instant {
    let now = Instant::now();
    TAKEN
        .lock()
        .unwrap()
        .push((std::thread::current().name().map(String::from), now));
    now
}

/// The stamp lands after the kill, on the watch's own thread; a gone pid cannot be
/// watched; a watch dropped before the exit ends its thread without stamping.
#[test]
fn the_moment_is_stamped_when_the_kernel_reports_the_exit() {
    let mut child = Command::new("/bin/sleep")
        .arg("600")
        .spawn()
        .expect("spawn");
    let pid = child.id();
    let watch = ExitMoment::watch(pid, stamp);
    let watch_error = std::io::Error::last_os_error();
    if watch.is_none()
        && cfg!(target_os = "linux")
        && watch_error.raw_os_error() == Some(38)
        && let Some(translator) = crate::process::translator()
    {
        child.kill().expect("kill the unwatchable child");
        child.wait().expect("reap the unwatchable child");
        println!(
            "NOT RUN under {translator}: exitwatch::moment_tests::the_moment_is_stamped_when_the_kernel_reports_the_exit: pidfd_open returned ENOSYS"
        );
        return;
    }
    if !cfg!(any(target_vendor = "apple", target_os = "linux")) {
        assert!(watch.is_none(), "no primitive: no watch");
        let _ = child.kill();
        let _ = child.wait();
        return;
    }
    let watch = watch.expect("watch a live child");
    assert_eq!(watch.get(), None, "alive: nothing stamped");
    let killed = Instant::now();
    child.kill().expect("SIGKILL");
    let _ = child.wait();
    let deadline = Instant::now() + Duration::from_secs(60);
    let at = loop {
        if let Some(at) = watch.get() {
            break at;
        }
        assert!(Instant::now() < deadline, "the exit was never stamped");
        std::thread::sleep(Duration::from_millis(5));
    };
    assert!(at >= killed, "stamped after the kill");
    let taken = TAKEN.lock().unwrap().clone();
    assert!(
        taken
            .iter()
            .any(|(name, when)| *name == Some(exit_moment_thread(pid)) && *when == at),
        "stamped on the watch's thread: {taken:?}"
    );

    let mut gone = Command::new("/usr/bin/true").spawn().expect("spawn");
    let gone_pid = gone.id();
    let _ = gone.wait();
    assert!(
        ExitMoment::watch(gone_pid, stamp).is_none(),
        "reaped: ESRCH"
    );
    assert!(ExitMoment::watch(0, stamp).is_none());
    assert!(ExitMoment::watch(1, stamp).is_none());

    let mut idle = Command::new("/bin/sleep")
        .arg("600")
        .spawn()
        .expect("spawn");
    let before = TAKEN.lock().unwrap().len();
    let watch = ExitMoment::watch(idle.id(), stamp).expect("watch");
    let thread = watch.thread_alive();
    drop(watch);
    let deadline = Instant::now() + Duration::from_secs(60);
    while thread.upgrade().is_some() {
        assert!(
            Instant::now() < deadline,
            "a dropped watch's thread never ended"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    let _ = idle.kill();
    let _ = idle.wait();
    assert_eq!(
        TAKEN.lock().unwrap().len(),
        before,
        "a watch dropped while its process lived stamps nothing"
    );
}
