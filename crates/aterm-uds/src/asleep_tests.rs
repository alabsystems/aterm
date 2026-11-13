// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{Asleep, asleep, queued_for, this_thread, threads};
use std::sync::mpsc;
use std::time::Duration;

/// The one-minute hang detector on every wait here that must end.
const HANG: Duration = Duration::from_secs(60);

/// Waits, up to [`HANG`], until `probe` answers, and returns that.
fn until<T>(what: &str, mut probe: impl FnMut() -> Option<T>) -> T {
    let begun = std::time::Instant::now();
    while begun.elapsed() < HANG {
        if let Some(found) = probe() {
            return found;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    panic!("{what}: not within {HANG:?}");
}

/// **A THREAD QUEUED FOR A HELD FILE LOCK IS ASLEEP IN `flock(2)` ON THAT FILE; ONE PARKED
/// ON A CHANNEL IS ASLEEP ELSEWHERE.** The waiter reads `Flock` on its own descriptor, and
/// [`queued_for`] finds it, and only it, by the file's path. The parked thread is the
/// control: the reading tells the kernel's lock queue from any other sleep, so it is no
/// "asleep at all" in disguise, and the parked thread is queued for nothing.
#[test]
fn a_thread_queued_for_a_held_lock_sleeps_in_flock_on_it_and_a_parked_one_elsewhere() {
    use std::os::fd::AsRawFd as _;
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let path = dir.path().join("lock");
    let held = std::fs::File::create(&path).expect("the holder's file");
    #[expect(
        clippy::disallowed_methods,
        reason = "a test's own hold, released by its explicit unlock below"
    )]
    held.lock().expect("the holder takes the lock");

    let (named, name) = mpsc::channel();
    let queued_path = path.clone();
    let queued = std::thread::spawn(move || {
        let file = std::fs::File::open(&queued_path).expect("the waiter's file");
        named
            .send((this_thread(), file.as_raw_fd()))
            .expect("name the waiter");
        #[expect(
            clippy::disallowed_methods,
            reason = "a test's own take, released by its explicit unlock below"
        )]
        file.lock().expect("the waiter takes the lock");
        file.unlock().expect("the waiter lets go");
    });
    let (waiter, fd) = name.recv_timeout(HANG).expect("the waiter's name");
    until("the waiter asleep in flock(2) on its descriptor", || {
        (asleep(waiter).expect("read the waiter") == Some(Asleep::Flock { fd })).then_some(())
    });
    assert!(
        threads().expect("this process's threads").contains(&waiter),
        "the listing holds the waiter"
    );
    assert_eq!(
        queued_for(&path).expect("the threads queued for the lock"),
        vec![waiter],
        "the waiter, and only it, is queued for the held file"
    );

    let (named, name) = mpsc::channel();
    let (wake, woken) = mpsc::channel::<()>();
    let parked = std::thread::spawn(move || {
        named.send(this_thread()).expect("name the parked thread");
        let _ = woken.recv();
    });
    let sleeper = name.recv_timeout(HANG).expect("the parked thread's name");
    let at = until("the parked thread asleep", || {
        asleep(sleeper).expect("read the parked thread")
    });
    assert!(
        matches!(at, Asleep::Elsewhere(_)),
        "CONTROL: a thread parked on a channel read {at:?}"
    );
    assert_eq!(
        queued_for(&path).expect("the threads queued for the lock"),
        vec![waiter],
        "CONTROL: the parked thread is queued for nothing"
    );
    wake.send(()).expect("wake the parked thread");
    parked.join().expect("the parked thread ends");

    held.unlock().expect("the holder lets go");
    queued.join().expect("the waiter took the lock and let go");
    assert_eq!(
        queued_for(&path).expect("the threads queued for the lock"),
        vec![],
        "none is queued once the lock is free"
    );
}

/// Linux's reading of `/proc/<pid>/task/<tid>/syscall`'s shapes.
#[test]
fn the_proc_syscall_line_names_flock_and_its_descriptor_another_call_or_none() {
    use super::low_c_int;
    use super::proc_syscall::{parse_with, sys_flock_for_kernel_arch};
    assert_eq!(parse_with("running\n", 73), None);
    assert_eq!(
        parse_with("-1 0x7ffc 0x401000\n", 73),
        Some(Asleep::Elsewhere("no call".into()))
    );
    assert_eq!(sys_flock_for_kernel_arch("x86_64\n"), Some(73));
    assert_eq!(sys_flock_for_kernel_arch("aarch64\n"), Some(32));
    assert_eq!(sys_flock_for_kernel_arch("unknown\n"), None);
    assert_eq!(low_c_int(0x1_0000_001a), 26);
    assert_eq!(low_c_int(0x8000_0000), i32::MIN);
    assert_eq!(low_c_int(0xffff_ffff_ffff_fffe), -2);
    for (flock_nr, other) in [(73, 32), (32, 73)] {
        assert_eq!(
            parse_with(
                &format!("{flock_nr} 0x1a 0x2 0x0 0x0 0x0 0x0 0x7ffc 0x401000\n"),
                flock_nr
            ),
            Some(Asleep::Flock { fd: 26 })
        );
        assert_eq!(
            parse_with(
                &format!("{flock_nr} 0x10000001a 0x2 0x0 0x0 0x0 0x0 0x7ffc 0x401000\n"),
                flock_nr
            ),
            Some(Asleep::Flock { fd: 26 }),
            "upper 32-bit register noise truncated to C int descriptor"
        );
        assert_eq!(
            parse_with(
                &format!("{flock_nr} 0xfffffffffffffffe 0x2 0x0 0x0 0x0 0x0 0x7ffc 0x401000\n"),
                flock_nr
            ),
            Some(Asleep::Flock { fd: -2 }),
            "sign-extended 64-bit negative descriptor preserved"
        );
        assert_eq!(
            parse_with(&format!("{flock_nr} 0x+1a 0x2\n"), flock_nr),
            Some(Asleep::Flock { fd: -1 }),
            "signed hex argument is rejected"
        );
        assert_eq!(
            parse_with(&format!("+{flock_nr} 0x1a 0x2\n"), flock_nr),
            Some(Asleep::Elsewhere(format!("+{flock_nr}"))),
            "plus-prefixed syscall number is not flock"
        );
        assert_eq!(
            parse_with(&format!("{other} 0x1a 0x2\n"), flock_nr),
            Some(Asleep::Elsewhere(format!("syscall {other}")))
        );
    }
    #[cfg(target_os = "linux")]
    {
        use super::imp::{parse, sys_flock};
        let live = sys_flock();
        let other = if live == 32 { 98 } else { 202 };
        assert_eq!(
            parse(&format!(
                "{live} 0x1a 0x2 0x0 0x0 0x0 0x0 0x7ffc 0x401000\n"
            )),
            Some(Asleep::Flock { fd: 26 })
        );
        assert_eq!(
            parse(&format!("{other} 0x1 0x2\n")),
            Some(Asleep::Elsewhere(format!("syscall {other}")))
        );
    }
}
