// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! N2's Linux readers read live (tracker package SD-02): the run queue, the process sweep's
//! zombies and stopped processes as the zombie book counts them, `procs_blocked`, the machine
//! type and RAPL, each through the API the panel reads and, where useful, held to a read of
//! `/proc` or `/sys` made apart from the reader. The fixture-tree tests (`linux_tests.rs`) hold
//! the parsers; these hold what a real kernel answers, in any Linux, a VM included. Each child
//! a law starts is its own, killed and reaped when the law ends, a failed one included; other
//! laws in the binary start children of this process too, so no law counts this process's
//! children whole.

use std::io::Write as _;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use aterm_spec::wait::{MUST_HAPPEN, until_within};
use aterm_uds::deadline::{StopReport, await_stopped, stop_state};
use aterm_vitals::blockage::ZombieBook;
use aterm_vitals::jobs::ZOMBIE_AGE_MS;

use crate::jobs::RunState;
use crate::machine_type::{friendly, is_code, is_placeholder};
use crate::runq::RunQueue;
use crate::{ProcessReading, Sweep, Why};

/// A child of this test, killed and reaped when it drops, a failed assertion's unwind included.
/// Once a reap has collected it, `kill` signals nothing and `wait` answers the status it kept.
struct Owned(aterm_uds::spawnfd::Child);

impl Drop for Owned {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// `/bin/sleep <secs>`, a child of this process, once it runs `sleep` (its `comm` says so:
/// the exec is past its point of no return, so a signal sent now reaches `sleep`).
fn sleep_child(secs: &str) -> Owned {
    let child = Owned(
        aterm_uds::spawnfd::command("/bin/sleep")
            .arg(secs)
            .spawn()
            .expect("/bin/sleep starts"),
    );
    let pid = child.0.id();
    until_within(MUST_HAPPEN, &format!("child {pid} runs sleep"), || {
        stat_fields(pid)
            .filter(|(name, _)| name == "sleep")
            .map(|_| ())
    });
    child
}

/// `pid`'s `comm` and state letter as `/proc/<pid>/stat` holds them, read apart from the
/// sweep: the name between the first `(` and the last `)`, and the field after it.
fn stat_fields(pid: u32) -> Option<(String, u8)> {
    let text = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let open = text.find('(')?;
    let (head, rest) = text.rsplit_once(')')?;
    let state = rest.split_whitespace().next()?.bytes().next()?;
    Some((head.get(open + 1..)?.to_owned(), state))
}

/// One live sweep of the process table, as the panel takes it.
fn sweep() -> Sweep {
    crate::processes().expect("Linux sweeps /proc")
}

/// `pid`'s row in `sweep`.
fn row(sweep: &Sweep, pid: u32) -> Option<&ProcessReading> {
    sweep.procs.iter().find(|p| p.pid == pid)
}

/// A sweep's zombies, each `(pid, parent)`, as the host hands them to the zombie book (its
/// `ProcState::Zombie` is the sweep's `RunState::Zombie`).
fn zombies(sweep: &Sweep) -> Vec<(u32, u32)> {
    sweep
        .procs
        .iter()
        .filter(|p| p.state == RunState::Zombie)
        .map(|p| (p.pid, p.ppid))
        .collect()
}

/// This process's real uid, for the task tables' per-user pair (Linux reads no per-user count).
fn uid() -> u32 {
    // SAFETY: `getuid` takes no argument and cannot fail.
    unsafe { libc::getuid() }
}

/// Sets its flag when it drops, so a thread spinning on it stops before a scope joins it, on a
/// failed assertion's unwind too.
struct SetOnDrop<'a>(&'a AtomicBool);

impl Drop for SetOnDrop<'_> {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

/// (1) THE RUN QUEUE FROM `/proc/loadavg` READS A COUNT: the task tables' run queue (the read
/// the panel takes) is a whole count, in thousandths, below the same read's task total, never
/// failed, unread-format or unsupported. A read of `/proc/loadavg` apart from the reader finds
/// its running count at least 1, the reading thread itself, which the reader takes off. While a
/// thread of this test spins, the queue comes to read at least 1, and every reading on the way
/// is a count.
#[test]
fn n2_linux_live_the_run_queue_reads_a_count() {
    let room = crate::task_room(uid());
    let RunQueue::Milli(milli) = room.run_queue else {
        panic!("the run queue reads a count: {room:?}");
    };
    assert_eq!(milli % 1000, 0, "Linux's queue is a count now: {room:?}");
    let threads = room.threads.expect("the same read's task total");
    assert!(
        milli / 1000 < threads,
        "the queue less its reader is under every task: {room:?}"
    );

    let text = std::fs::read_to_string("/proc/loadavg").expect("/proc/loadavg reads");
    let running: u64 = text
        .split_whitespace()
        .nth(3)
        .and_then(|f| f.split_once('/'))
        .and_then(|(running, _)| running.parse().ok())
        .unwrap_or_else(|| panic!("/proc/loadavg's fourth field is running/total: {text:?}"));
    assert!(running >= 1, "the reading thread counts itself: {text:?}");

    let stop = AtomicBool::new(false);
    let spinning = AtomicBool::new(false);
    std::thread::scope(|s| {
        let _stop = SetOnDrop(&stop);
        s.spawn(|| {
            spinning.store(true, Ordering::Release);
            let mut x = 0u64;
            while !stop.load(Ordering::Acquire) {
                x = std::hint::black_box(x.wrapping_add(1));
            }
        });
        until_within(MUST_HAPPEN, "the spinner runs", || {
            spinning.load(Ordering::Acquire).then_some(())
        });
        let milli = until_within(
            MUST_HAPPEN,
            "the run queue counts the spinning thread",
            || match crate::task_room(uid()).run_queue {
                RunQueue::Milli(m) => (m >= 1000).then_some(m),
                other => panic!("every reading is a count: {other:?}"),
            },
        );
        assert_eq!(milli % 1000, 0, "{milli}");
    });
}

/// (2) A CHILD LEFT UNREAPED IS A ZOMBIE UNDER THIS PROCESS, AND THE BOOK COUNTS IT ONCE AGED:
/// once the kernel reports the child's exit (unreaped: `WNOWAIT`), one live sweep reads its row
/// `Zombie` under this test's pid, with its own name, and `/proc/<pid>/stat` says `Z`. A
/// [`ZombieBook`] noted with that sweep and a second one, its clock given (no sleep), keeps the
/// first sighting, counts no pile a millisecond short of [`ZOMBIE_AGE_MS`], and at that age
/// counts under this pid exactly the zombies both sweeps saw under it, the child among them
/// (other laws' children may be zombies of this process too). NEGATIVE CONTROL: once reaped, the
/// child is in no sweep, and a book noted with one forgets it.
#[test]
fn n2_linux_live_an_unreaped_child_is_a_zombie_the_book_counts_once_aged() {
    let me = std::process::id();
    let mut child = sleep_child("0");
    let pid = child.0.id();
    until_within(MUST_HAPPEN, &format!("child {pid} exits"), || {
        child
            .0
            .exited()
            .expect("waitid on our own child")
            .then_some(())
    });

    let first = sweep();
    let z = row(&first, pid).unwrap_or_else(|| panic!("the zombie {pid} is swept"));
    assert_eq!(
        (z.state, z.ppid, z.name.as_str()),
        (RunState::Zombie, me, "sleep"),
        "{z:?}"
    );
    assert_eq!(
        stat_fields(pid).map(|(_, state)| state),
        Some(b'Z'),
        "/proc says zombie"
    );
    let zombie_start = z.start;

    let t0: u64 = 1_000_000;
    let mut book = ZombieBook::default();
    let seen_first = zombies(&first);
    assert!(seen_first.contains(&(pid, me)), "{seen_first:?}");
    book.note(t0, seen_first.iter().copied());
    assert_eq!(book.seen(pid), Some((t0, me)));
    assert!(book.piles(t0).is_empty(), "first seen now: not yet aged");

    let second = sweep();
    let seen_second = zombies(&second);
    assert!(
        seen_second.contains(&(pid, me)),
        "still unreaped: {seen_second:?}"
    );
    let t1 = t0 + ZOMBIE_AGE_MS;
    book.note(t1, seen_second.iter().copied());
    assert_eq!(
        book.seen(pid),
        Some((t0, me)),
        "the second sweep keeps the first sighting"
    );
    assert!(
        book.piles(t1 - 1).is_empty(),
        "a millisecond short of its age, no zombie counts"
    );
    let both = seen_first
        .iter()
        .filter(|z| z.1 == me && seen_second.contains(z))
        .count();
    assert_eq!(
        book.piles(t1).get(&me).copied(),
        Some(u32::try_from(both).expect("a count")),
        "at its age, every zombie of this process both sweeps saw: {seen_first:?} {seen_second:?}"
    );

    child.0.wait().expect("reap the zombie");
    let third = sweep();
    assert!(
        !third
            .procs
            .iter()
            .any(|p| p.pid == pid && p.start == zombie_start),
        "the reaped child is in no sweep"
    );
    book.note(t1 + 1, zombies(&third));
    assert_eq!(book.seen(pid), None, "the book forgets the reaped child");
}

/// (3a) `/proc/stat`'S `procs_blocked` READS A COUNT: the probe the panel samples answers none
/// before its first read and a count from it, and the file, read whole apart from the reader,
/// holds the line within the reader's cap.
#[test]
fn n2_linux_live_procs_blocked_reads_a_count() {
    let mut probe = crate::Probe::new();
    assert_eq!(probe.blocked(), None, "no count before a read");
    assert!(probe.core_ticks().is_some(), "/proc/stat reads");
    let blocked = probe.blocked();
    let text = std::fs::read_to_string("/proc/stat").expect("/proc/stat reads");
    let at = text.find("\nprocs_blocked ").map(|i| i + 1);
    assert!(
        blocked.is_some(),
        "procs_blocked reads a count: the line at byte {at:?} of {}, the reader's cap {}",
        text.len(),
        crate::linux::STAT_CAP
    );
    let line = at.and_then(|i| text[i..].lines().next());
    let count = line
        .and_then(|l| l.strip_prefix("procs_blocked "))
        .and_then(|n| n.trim().parse::<u64>().ok());
    assert!(
        count.is_some(),
        "an apart read finds procs_blocked a count: {line:?}"
    );
}

/// (3b) A CHILD STOPPED BY `SIGSTOP` READS STOPPED IN THE LIVE SWEEP, which reads each
/// process's own state field: once the kernel reports the stop, one sweep reads the child's row
/// `Stopped` under this pid, `/proc/<pid>/stat` says `T`, and this process's own row in that
/// sweep is not stopped. NEGATIVE CONTROL: once the kernel reports it continued (`SIGCONT`), a
/// sweep reads it sleeping or running.
#[test]
fn n2_linux_live_a_stopped_child_reads_stopped_in_the_sweep() {
    let me = std::process::id();
    let child = sleep_child("120");
    let pid = child.0.id();
    let signed = i32::try_from(pid).expect("a pid");

    // SAFETY: `kill` takes no pointers; `signed` is this test's own unreaped child.
    let sent = unsafe { libc::kill(signed, libc::SIGSTOP) };
    assert_eq!(sent, 0, "SIGSTOP: {}", std::io::Error::last_os_error());
    await_stopped(signed, MUST_HAPPEN).expect("the kernel reports the child stopped");
    let stopped = sweep();
    let r = row(&stopped, pid).unwrap_or_else(|| panic!("the stopped child {pid} is swept"));
    assert_eq!(
        (r.state, r.ppid, r.name.as_str()),
        (RunState::Stopped, me, "sleep"),
        "{r:?}"
    );
    assert_eq!(
        stat_fields(pid).map(|(_, state)| state),
        Some(b'T'),
        "/proc says stopped"
    );
    let mine = row(&stopped, me).expect("this process is swept");
    assert_ne!(
        mine.state,
        RunState::Stopped,
        "the parent's row reads its own state, not its child's: {mine:?}"
    );

    // SAFETY: as above.
    let sent = unsafe { libc::kill(signed, libc::SIGCONT) };
    assert_eq!(sent, 0, "SIGCONT: {}", std::io::Error::last_os_error());
    until_within(
        MUST_HAPPEN,
        "the kernel reports the child continued",
        || (stop_state(signed) == Err(StopReport::Continued)).then_some(()),
    );
    let resumed = sweep();
    let r = row(&resumed, pid).unwrap_or_else(|| panic!("the continued child {pid} is swept"));
    assert!(
        matches!(r.state, RunState::Sleeping | RunState::Running),
        "a continued sleep is not stopped: {r:?}"
    );
    drop(child);
}

/// (4) THE MACHINE TYPE IS A NAME OR NO NAME, NEVER A CODE OR A PLACEHOLDER (M-NAME's VM case):
/// what the panel reads ([`crate::machine_type::read`], from the device tree or DMI) is the
/// first field this law reads apart from it that names the machine (the device tree's model
/// where it is no `compatible` entry, else DMI's product name, version and family in turn), with
/// at most the vendor word before it: trimmed, with no NUL or newline from the files, led by no
/// code, no `compatible` identifier, and equal to no field that is a code or a placeholder;
/// where no field names the machine, it names none. Its identifier, where read, is no
/// placeholder. The reading is written past libtest's capture, so a passing run's log records
/// what this machine's firmware says.
#[test]
fn n2_linux_live_the_machine_type_is_a_name_or_none() {
    let ty = crate::machine_type::read().expect("Linux has a machine-type reader");
    let raw = |path: &str| {
        std::fs::read(path)
            .ok()
            .map(|b| String::from_utf8_lossy(&b).into_owned())
    };
    let tree_model = raw("/proc/device-tree/model");
    let dmi = ["product_name", "product_version", "product_family"]
        .map(|f| (f, raw(&format!("/sys/class/dmi/id/{f}"))));
    let vendor = raw("/sys/class/dmi/id/sys_vendor");
    // Past libtest's capture, so a passing run's log keeps what this machine's firmware says.
    let _ = writeln!(
        std::io::stderr(),
        "machine type {ty:?}; device tree model {tree_model:?}; DMI {dmi:?}, sys_vendor {vendor:?}"
    );
    // Each field as the ladder reads it: the device tree's string up to its NUL, DMI's trimmed.
    let tree_first = tree_model
        .as_deref()
        .and_then(|m| m.split('\0').next())
        .map(str::trim);
    let fields: Vec<&str> = tree_first
        .into_iter()
        .chain(dmi.iter().filter_map(|(_, t)| t.as_deref().map(str::trim)))
        .chain(vendor.as_deref().map(str::trim))
        .collect();

    // What names the machine, read here apart from the reader: the device tree's model where it
    // is no `compatible` entry, else the first of DMI's product name, version and family.
    let compatible: Vec<String> = raw("/proc/device-tree/compatible")
        .map(|c| {
            c.split('\0')
                .map(str::trim)
                .filter(|e| !e.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    let tree_name = tree_first
        .filter(|m| !compatible.iter().any(|c| c == m) && !crate::machine_type::is_compatible(m))
        .and_then(friendly);
    let expected = tree_name.or_else(|| {
        dmi.iter()
            .find_map(|(_, t)| t.as_deref().and_then(friendly))
    });
    match (&ty.name, expected) {
        (Some(name), Some(field)) => {
            assert!(
                name == field || name.ends_with(&format!(" {field}")),
                "{name:?} is the first field that names the machine, {field:?}, with at most \
                 the vendor word before it"
            );
            assert_eq!(name.trim(), name, "trimmed: {name:?}");
            assert!(
                !name.chars().any(char::is_control),
                "no NUL or newline from the files: {name:?}"
            );
            assert!(
                !is_code(name) && !is_placeholder(name),
                "not a code or a placeholder: {name:?}"
            );
            let lead = name.split_whitespace().next().expect("a word");
            assert!(!is_code(lead), "led by no code: {name:?}");
            assert!(
                !compatible.iter().any(|c| c == name)
                    && !(name.contains(',') && !name.contains(char::is_whitespace)),
                "{name:?} is a `compatible` identifier, not a name"
            );
            for field in fields.iter().filter(|f| is_code(f) || is_placeholder(f)) {
                assert!(
                    !name.eq_ignore_ascii_case(field),
                    "{name:?} is the field {field:?}, a code or a placeholder"
                );
            }
        }
        (None, None) => {}
        (got, want) => panic!("the reader named {got:?}, the fields name {want:?}"),
    }
    if let Some(family) = &ty.family {
        assert_eq!(friendly(family), Some(family.as_str()), "{ty:?}");
        assert_ne!(ty.name.as_ref(), Some(family), "{ty:?}");
    }
    if let Some(id) = &ty.model_id {
        assert!(
            !is_placeholder(id) && !id.contains('\0'),
            "the identifier is no placeholder: {id:?}"
        );
    }
    assert_eq!(ty.model, None, "the long form is a Mac's: {ty:?}");
}

/// (5) RAPL READS UNSUPPORTED WHERE `/sys/class/powercap` HAS NO `package-` ZONE (a VM):
/// [`crate::rapl::Rapl`] opened on `/`, as the panel opens it, answers [`Why::Absent`] (the
/// panel's `no_rapl`, not supported) at every read, never a number or a failed read. Where
/// this machine has a package zone the reader reads (a top-level `intel-rapl:N`), the law says
/// so and holds the reading to what its counter allows: a number (none at the first read, which
/// leaves the base; watts at the second, its clock given) where `energy_uj` reads, and
/// [`Why::NeedsPrivilege`] (`root`) where it is root's alone (Linux 5.10 on, CVE-2020-8694).
#[test]
fn n2_linux_live_rapl_reads_unsupported_without_a_package_zone() {
    let zones: Vec<(String, std::path::PathBuf)> = std::fs::read_dir("/sys/class/powercap")
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let dir = e.file_name().into_string().ok()?;
            let name = std::fs::read_to_string(e.path().join("name")).ok()?;
            name.trim().starts_with("package-").then(|| (dir, e.path()))
        })
        .collect();
    let mut rapl = crate::rapl::Rapl::open(Path::new("/"));
    let t0 = Instant::now();
    let first = rapl.watts(t0);
    let second = rapl.watts(t0 + Duration::from_secs(1));

    if zones.is_empty() {
        assert_eq!(
            (first, second),
            (Err(Why::Absent), Err(Why::Absent)),
            "no package zone: unsupported at every read"
        );
        return;
    }
    let read: Vec<&(String, std::path::PathBuf)> = zones
        .iter()
        .filter(|(dir, _)| {
            dir.strip_prefix("intel-rapl:")
                .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        })
        .collect();
    let _ = writeln!(
        std::io::stderr(),
        "this machine has package zones {:?}, of which the reader reads {:?}: first {first:?}, second {second:?}",
        zones.iter().map(|(d, _)| d).collect::<Vec<_>>(),
        read.iter().map(|(d, _)| d).collect::<Vec<_>>()
    );
    let Some((_, dir)) = read.first() else {
        assert_eq!(
            (first, second),
            (Err(Why::Absent), Err(Why::Absent)),
            "no top-level intel-rapl package zone: unsupported at every read"
        );
        return;
    };
    match std::fs::read_to_string(dir.join("energy_uj")) {
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => assert_eq!(
            (first, second),
            (
                Err(Why::NeedsPrivilege("root")),
                Err(Why::NeedsPrivilege("root"))
            ),
            "the counter is root's alone"
        ),
        Ok(_) => {
            assert_eq!(first, Ok(None), "the first read leaves the base");
            let watts = second
                .expect("the counter reads")
                .expect("the second read gives watts");
            assert!(watts.is_finite() && watts >= 0.0, "{watts}");
        }
        Err(e) => panic!("energy_uj of {dir:?} reads: {e}"),
    }
}
