// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A stage's memory meter as its children's clocks feed it (VERIFY-MEM): reads a stand-in host
//! answers, and one read of a real child's tree.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use super::{Clock, Host};
use crate::memory::Meter;

const GIB: u64 = 1 << 30;
const MIB: u64 = 1 << 20;

/// A wait for something that must happen: a hang detector, never a latency budget.
const HANG_DETECTOR: Duration = Duration::from_secs(60);

/// A host whose reads of a group answer `reads` in turn, then nothing.
#[derive(Debug)]
struct Scripted {
    reads: Mutex<VecDeque<Option<Vec<aterm_sysprobe::MemberUsage>>>>,
}

impl Scripted {
    fn of(reads: Vec<Option<Vec<aterm_sysprobe::MemberUsage>>>) -> Scripted {
        Scripted {
            reads: Mutex::new(reads.into()),
        }
    }
}

impl Host for Scripted {
    fn group_usage(&self, _: u32) -> Option<Vec<aterm_sysprobe::MemberUsage>> {
        self.reads
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pop_front()
            .flatten()
    }

    fn idle(&self) -> Option<Duration> {
        None
    }

    fn cores(&self) -> u32 {
        1
    }
}

/// A group's members, each holding its GiB of `held` now and at most `peak` GiB at any time.
fn members(held: &[u64], peak: u64) -> Option<Vec<aterm_sysprobe::MemberUsage>> {
    Some(
        held.iter()
            .enumerate()
            .map(|(i, gib)| aterm_sysprobe::MemberUsage {
                pid: u32::try_from(i).expect("small") + 100,
                start: 1,
                footprint: gib * GIB,
                peak_footprint: (gib * GIB).max(peak * GIB),
                ..aterm_sysprobe::MemberUsage::default()
            })
            .collect(),
    )
}

/// `reads` read by one child's clock of a stage metered by `meter`, the clock ended after them.
fn clocked(meter: &Arc<Meter>, pgid: u32, reads: Vec<Option<Vec<aterm_sysprobe::MemberUsage>>>) {
    let host = Scripted::of(reads);
    let n = host
        .reads
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .len();
    let mut clock = Clock::new(None, &host, pgid, Some(Arc::clone(meter)));
    for _ in 0..n {
        clock.read_usage();
    }
}

/// A stage's peak is the most its tree held at one read: reads of 3 + 4, then 5, then 2 + 2 GiB
/// book 7 GiB. A member's lifetime peak the kernel kept raises it (a member read at 1 GiB that
/// held 9). NEGATIVE CONTROL: a host whose groups are unread books nothing, so no peak is kept.
#[test]
fn a_stages_peak_is_the_most_its_tree_held_at_one_read() {
    let meter = Meter::new();
    assert_eq!(meter.peak(), None, "nothing read yet");
    clocked(
        &meter,
        77,
        vec![members(&[3, 4], 0), members(&[5], 0), members(&[2, 2], 0)],
    );
    assert_eq!(meter.peak(), Some(7 * GIB));

    let lifetime = Meter::new();
    clocked(&lifetime, 77, vec![members(&[1], 9)]);
    assert_eq!(lifetime.peak(), Some(9 * GIB));

    let unread = Meter::new();
    clocked(&unread, 77, vec![None, members(&[5], 0)]);
    assert_eq!(unread.peak(), None, "the first unread read ends the reads");
}

/// A stage's children that run at once hold their memory together: one tree read at 3 GiB and
/// another at 4 GiB while it still runs book 7 GiB. NEGATIVE CONTROL: once the first child's
/// clock has ended, what it held is the stage's no longer, and the second's 4 GiB reads 4.
#[test]
fn a_stages_live_trees_are_held_together_until_each_ends() {
    let together = Meter::new();
    let first = Scripted::of(vec![members(&[3], 0)]);
    let mut a = Clock::new(None, &first, 1, Some(Arc::clone(&together)));
    a.read_usage();
    clocked(&together, 2, vec![members(&[4], 0)]);
    drop(a);
    assert_eq!(together.peak(), Some(7 * GIB));

    let apart = Meter::new();
    clocked(&apart, 1, vec![members(&[3], 0)]);
    clocked(&apart, 2, vec![members(&[4], 0)]);
    assert_eq!(apart.peak(), Some(4 * GIB));
}

/// A stage that started no child held nothing, and keeps that: a doctest stage over crates with
/// no fenced Rust block keeps 0, so its next run claims the 1 GiB margin, not the cold 12 GiB of
/// a 24 GiB Mac. A stage whose children started and were never read holds no figure, and one
/// read holds what was read. NEGATIVE CONTROL: a clock that does not mark its child started
/// leaves the unread stage reading 0, as if it had started none.
#[test]
fn a_stage_that_started_no_child_keeps_that_it_held_nothing() {
    let none = Meter::new();
    assert_eq!((none.peak(), none.held()), (None, Some(0)));
    let unread = Meter::new();
    clocked(&unread, 77, vec![None]);
    assert_eq!(
        (unread.peak(), unread.held()),
        (None, None),
        "children started and no read booked them"
    );
    let read = Meter::new();
    clocked(&read, 77, vec![members(&[2], 0)]);
    assert_eq!(read.held(), Some(2 * GIB));

    let dir = aterm_tempfile::tempdir().expect("scratch");
    let book = crate::memory::Book::at(
        Some(dir.path().join("stage-memory.json")),
        Some(24 * GIB),
        Some(21 * GIB),
    );
    let doctests = crate::plan::StageSpec {
        id: crate::plan::StageId::Doctests,
        title: "doctests".to_owned(),
        lane: crate::plan::Lane::AfterCompile,
        exclusive: false,
        after_stages: Vec::new(),
        mem: 0,
    };
    let key = crate::memory::Key::of(
        doctests.id,
        &crate::scope::Scope::crate_only("aterm-capacity"),
    );
    assert_eq!(book.need(&doctests, &key).bytes, 12 * GIB, "cold");
    book.keep(&doctests, &key, none.held().expect("held nothing"))
        .expect("kept");
    assert_eq!(book.need(&doctests, &key).bytes, GIB, "the margin alone");
}

/// Selects the memory hog's stand-in: set, this test binary touches that many MiB, says
/// `touched`, and holds them until its standard input ends.
const HOG_ENV: &str = "ATERM_VERIFY_MEMORY_HOG_MIB";

/// The memory hog: touches each page of [`HOG_ENV`]'s MiB, says so, and keeps them until its
/// standard input ends (a read, never a sleep). Without [`HOG_ENV`] it returns at once.
#[test]
fn a_memory_hog_stand_in() {
    let Some(mib) = std::env::var(HOG_ENV)
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
    else {
        return;
    };
    let mut hog = vec![0u8; mib << 20];
    for page in hog.chunks_mut(4096) {
        page[0] = 1;
    }
    std::hint::black_box(&hog);
    println!("touched");
    let mut line = String::new();
    let _ = std::io::stdin().read_line(&mut line);
    std::hint::black_box(&hog);
}

/// One read of a real child's tree books what it touched: the memory hog's stand-in, in a group
/// of its own, read once after it said it touched 256 MiB, books at least 256 MiB into the
/// stage's meter. NEGATIVE CONTROL: the stand-in touching nothing books less than 64 MiB.
#[cfg(target_os = "macos")]
#[test]
fn a_real_childs_tree_books_what_it_touched() {
    use aterm_uds::spawnfd::Stdio;
    let booked = |mib: u64| {
        let mut child =
            aterm_uds::spawnfd::command(std::env::current_exe().expect("the test binary"))
                .args([
                    "--exact",
                    "exec::memory_tests::a_memory_hog_stand_in",
                    "--nocapture",
                ])
                .env(HOG_ENV, mib.to_string())
                .process_group(0)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .expect("the hog's stand-in");
        aterm_uds::deadline::prints(&mut child, "touched", HANG_DETECTOR);
        let meter = Meter::new();
        let mut clock = Clock::new(
            None,
            &super::LIVE_HOST,
            child.id(),
            Some(Arc::clone(&meter)),
        );
        clock.read_usage();
        drop(clock);
        drop(child.stdin.take());
        aterm_uds::deadline::kill_within(&mut child, HANG_DETECTOR);
        meter.peak().expect("the child's group is read")
    };
    let touched = booked(256);
    assert!(
        touched >= 256 * MIB,
        "{} MiB booked for 256 touched",
        touched / MIB
    );
    let idle = booked(0);
    assert!(
        idle < 64 * MIB,
        "{} MiB booked for none touched",
        idle / MIB
    );
}

/// A child is read for its stage's meter as it starts, not only once half a second has passed:
/// the first tick after a child of 3 GiB starts books 3 GiB, so a child that ends within its
/// first half second is still booked. A read that finds the group gone (no member) books
/// nothing, so a child that ended unread holds no figure rather than 0. NEGATIVE CONTROL: the
/// same first tick with no meter books nothing, and so does a meter its clock never ticked.
#[test]
fn a_child_is_read_for_its_meter_as_it_starts() {
    let meter = Meter::new();
    let host = Scripted::of(vec![members(&[3], 0)]);
    let mut clock = Clock::new(None, &host, 9, Some(Arc::clone(&meter)));
    clock.tick();
    drop(clock);
    assert_eq!(meter.peak(), Some(3 * GIB), "read as it starts");

    let empty = Meter::new();
    clocked(&empty, 77, vec![Some(Vec::new())]);
    assert_eq!(
        (empty.peak(), empty.held()),
        (None, None),
        "a read of no member books nothing"
    );

    let untouched = Meter::new();
    let host = Scripted::of(vec![members(&[3], 0)]);
    let clock = Clock::new(None, &host, 9, Some(Arc::clone(&untouched)));
    drop(clock);
    assert_eq!(untouched.peak(), None, "never ticked");
}

/// Selects the FIFO hog's stand-in: set to a directory, this test binary touches 256 MiB, opens
/// the FIFO `fifo` there (read and write, so its open waits for nobody), says `ready` in a file
/// there, and ends once a byte comes through the FIFO: a read, never a sleep.
const FIFO_HOG: &str = "ATERM_VERIFY_FIFO_HOG";

/// The FIFO hog ([`FIFO_HOG`]); without it, it returns at once.
#[test]
fn a_fifo_hog_stand_in() {
    use std::io::Read as _;
    let Some(dir) = std::env::var_os(FIFO_HOG).map(std::path::PathBuf::from) else {
        return;
    };
    let mut hog = vec![0u8; 256 << 20];
    for page in hog.chunks_mut(4096) {
        page[0] = 1;
    }
    std::hint::black_box(&hog);
    let mut fifo = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(dir.join("fifo"))
        .expect("the FIFO");
    std::fs::write(dir.join("ready"), "ready").expect("said ready");
    let mut byte = [0u8; 1];
    let _ = fifo.read(&mut byte);
    std::hint::black_box(&hog);
}

/// Lets the FIFO hog go when dropped, if it still waits: a write that opens the FIFO without
/// waiting, so a hog that already ended (no reader) leaves nothing to do.
#[cfg(target_os = "macos")]
struct Release(std::path::PathBuf);

#[cfg(target_os = "macos")]
impl Drop for Release {
    fn drop(&mut self) {
        use std::io::Write as _;
        use std::os::unix::fs::OpenOptionsExt as _;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&self.0)
        {
            let _ = f.write_all(b"g");
        }
    }
}

/// A child an in-process walk starts through its output executor ([`super::bounded`], the
/// command scope's owned child) books into the stage's meter as it runs: the FIFO hog, started
/// by a walk under a stage's meter, is booked at 256 MiB or more while it waits, within the hang
/// detector, and the walk then ends with the child's output. NEGATIVE CONTROL: before
/// the scope carried the stage's meter to the walk's thread and its owned child read its group,
/// no read booked it, and the hang detector fails by name.
#[cfg(target_os = "macos")]
#[test]
fn a_bounded_walks_output_child_books_into_its_stages_meter() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let fifo = dir.path().join("fifo");
    let c = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).expect("a path");
    // SAFETY: `mkfifo` reads the NUL-terminated path `c` owns, alive for the call.
    assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0, "the FIFO");
    let release = Release(fifo);
    let meter = Meter::new();
    let theirs = Arc::clone(&meter);
    let at = dir.path().to_path_buf();
    let walk = std::thread::spawn(move || {
        let _metering = crate::memory::metering(Some(theirs));
        super::bounded(Some(HANG_DETECTOR), move || {
            let mut cmd =
                aterm_uds::spawnfd::command(std::env::current_exe().expect("the test binary"));
            cmd.args([
                "--exact",
                "exec::memory_tests::a_fifo_hog_stand_in",
                "--nocapture",
            ])
            .env(FIFO_HOG, &at);
            aterm_uds::output::output(&mut cmd)
        })
    });
    aterm_spec::wait::until_within(
        HANG_DETECTOR,
        "the walk's child booked at 256 MiB or more",
        || meter.peak().filter(|p| *p >= 256 * MIB),
    );
    aterm_spec::wait::until_within(HANG_DETECTOR, "the hog waiting on its FIFO", || {
        dir.path().join("ready").exists().then_some(())
    });
    drop(release);
    aterm_spec::wait::until_within(HANG_DETECTOR, "the walk's end", || {
        walk.is_finished().then_some(())
    });
    let out = walk
        .join()
        .expect("the walk's thread")
        .expect("the walk ended")
        .expect("the child's output");
    assert!(out.status.success(), "{:?}", out.status);
    assert!(meter.peak().is_some_and(|p| p >= 256 * MIB));
}
