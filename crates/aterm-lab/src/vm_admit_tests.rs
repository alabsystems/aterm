// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

use std::io::{BufRead as _, BufReader, Write as _};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use aterm_capacity::{Fixed, Proc, Procs, Reading};
use aterm_uds::spawnfd::Stdio;

/// The one-minute hang detector for what must happen.
const MUST: Duration = Duration::from_secs(60);

/// The stand-in's ledger directory.
const CLAIM_ENV: &str = "ATERM_LAB_TEST_VM_CLAIM";
/// The stand-in holding lima's boot claim.
const CLAIM_STAND_IN: &str = "vm_admit::tests::a_lima_boot_claim_stand_in";
/// What the stand-in says once it holds the claim.
const HOLDS: &str = "the stand-in holds lima's boot claim";
/// The disk whose holders the claim stand-in credits its claim with, told [`CREDIT`].
const DISK_ENV: &str = "ATERM_LAB_TEST_VM_DISK";
/// The line that tells the claim stand-in to credit its claim.
const CREDIT: &str = "credit";
/// What the claim stand-in says once it has credited its claim, and with which pids.
const CREDITED: &str = "the stand-in credited lima's claim with";
/// The stand-in for a VM's process, which holds the VM's disk.
const VM_STAND_IN: &str = "vm_admit::tests::a_vm_process_stand_in";
/// The disk the VM stand-in holds open.
const VM_ENV: &str = "ATERM_LAB_TEST_VM_PROCESS";
/// What the VM stand-in says once it holds its disk.
const VM_UP: &str = "the VM stand-in holds its disk";

/// A pace that looks often and says its wait line once a minute.
fn quick() -> Pace {
    Pace {
        poll: Duration::from_millis(10),
        line_every: Duration::from_secs(60),
    }
}

/// Not a test alone: as [`CLAIM_STAND_IN`] it takes lima's boot claim in the ledger at
/// [`CLAIM_ENV`] on a machine with room and says [`HOLDS`]. Each [`CREDIT`] line on stdin
/// credits the claim with the processes holding the disk at [`DISK_ENV`] ([`vm_procs`]),
/// as a run does once its VM has booted, and says [`CREDITED`] with them; at any other line
/// it dies by its own `SIGKILL`, the claim still held.
#[test]
fn a_lima_boot_claim_stand_in() {
    let Some(dir) = std::env::var_os(CLAIM_ENV) else {
        return;
    };
    let mut held = admit_in(
        &Ledger::at(dir),
        &Fixed::new(Reading::of_gib(48, 48, 14)),
        LIMA.need("lab linux proof", true),
        &LIMA,
        Admit::Wait,
        quick(),
        &mut |_| {},
        &|| Ok(()),
    )
    .unwrap_or_else(|f| panic!("lima's claim: {:?}", f.msg));
    println!("{HOLDS}");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    while std::io::stdin().read_line(&mut line).is_ok_and(|n| n > 0) && line.trim() == CREDIT {
        let disk = std::env::var_os(DISK_ENV).map(PathBuf::from);
        let pids = disk.map(|d| vm_procs(&d, &[])).unwrap_or_default();
        held.credit("linux", &pids);
        println!("{CREDITED} {pids:?}");
        let _ = std::io::stdout().flush();
        line.clear();
    }
    let _kept = held;
    aterm_uds::process::kill_self_now()
}

/// Not a test alone: as [`VM_STAND_IN`] it holds the disk at [`VM_ENV`] open, as a VM's
/// process holds its disk, says [`VM_UP`], and ends at its first line on stdin.
#[test]
fn a_vm_process_stand_in() {
    let Some(disk) = std::env::var_os(VM_ENV) else {
        return;
    };
    let _disk = std::fs::File::open(&disk).expect("the disk");
    println!("{VM_UP}");
    let _ = std::io::stdout().flush();
    let _ = std::io::stdin().read_line(&mut String::new());
}

/// The test binary run as the stand-in `name` with `env`, its stdin piped, each line it says
/// sent on the receiver.
fn stand_in(
    name: &str,
    env: &[(&str, &std::ffi::OsStr)],
) -> (aterm_uds::spawnfd::Child, Receiver<String>) {
    let mut cmd = aterm_uds::spawnfd::command(std::env::current_exe().expect("this test"));
    cmd.args(["--exact", name, "--nocapture", "--test-threads=1"]);
    for (k, v) in env {
        cmd.env(k, v);
    }
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("the stand-in starts");
    let mut said = BufReader::new(child.stdout.take().expect("its stdout"));
    let (lines, heard) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        while said.read_line(&mut line).is_ok_and(|n| n > 0) {
            let _ = lines.send(std::mem::take(&mut line));
        }
    });
    (child, heard)
}

/// The first line `heard` that holds `what`, within the one-minute hang detector.
fn hear(heard: &Receiver<String>, what: &str) -> String {
    let end = Instant::now() + MUST;
    loop {
        let line = heard
            .recv_timeout(end.saturating_duration_since(Instant::now()))
            .unwrap_or_else(|_| panic!("the stand-in says {what:?}"));
        if line.contains(what) {
            return line;
        }
    }
}

/// A VM's process runs outside the tree of the run that booted it. Lima's boot claim (16 GiB)
/// is held by another process, and lima's VM process (a stand-in holding the VM's disk, not
/// that process's child) holds 14 GiB of it on a 48 GiB Mac with 30 GiB available. With the
/// claim not yet credited, Windows' boot waits, short by 2 GiB (the negative control), and an
/// interrupt ends that wait as NOT RUN, carrying its signal, naming what it waited for and
/// that nothing booted, its claim gone from the queue. Once lima's holder credits its claim
/// with whatever holds the VM's disk ([`vm_procs`] finds the stand-in), the same boot on the
/// same reading is admitted at once, no wait line said.
#[test]
fn an_interrupt_ends_the_wait_and_a_credited_vm_is_not_reserved_twice() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("lab-vm-credit-")
        .tempdir()
        .expect("a scratch ledger");
    let ledger = Ledger::at(dir.path().join("capacity"));
    let disk = dir.path().join("disk.img");
    std::fs::write(&disk, b"disk").expect("a disk");
    let (mut vm, vm_said) = stand_in(VM_STAND_IN, &[(VM_ENV, disk.as_os_str())]);
    hear(&vm_said, VM_UP);
    let (mut lima, lima_said) = stand_in(
        CLAIM_STAND_IN,
        &[
            (CLAIM_ENV, ledger.dir().as_os_str()),
            (DISK_ENV, disk.as_os_str()),
        ],
    );
    hear(&lima_said, HOLDS);
    let machine = Arc::new(Fixed::new(Reading::of_gib(48, 30, 14)));
    machine.set_procs(Some(Procs::of(vec![Proc {
        pid: vm.id(),
        ppid: 1,
        charged: 14 << 30,
        footprint: 14 << 30,
        written: 0,
        cpu_ns: 0,
        runnable_ns: 0,
    }])));

    let stop = Arc::new(AtomicBool::new(false));
    let (lines, heard) = std::sync::mpsc::channel::<String>();
    let (ended, end) = std::sync::mpsc::channel();
    {
        let (ledger, machine, stop) = (ledger.clone(), Arc::clone(&machine), Arc::clone(&stop));
        std::thread::spawn(move || {
            let got = admit_in(
                &ledger,
                &*machine,
                WINDOWS.need("lab windows up", true),
                &WINDOWS,
                Admit::Wait,
                quick(),
                &mut |l| {
                    let _ = lines.send(l.to_owned());
                },
                &|| {
                    if stop.load(Ordering::SeqCst) {
                        Err(crate::stop::Stopped(libc::SIGINT))
                    } else {
                        Ok(())
                    }
                },
            );
            let _ = ended.send(got.map(|a| a.jobs));
        });
    }
    let first = heard.recv_timeout(MUST).expect("Windows' first wait line");
    for named in [
        "memory short by 2 GiB",
        "30 GiB available less 16 GiB reserved",
        &format!("pid {} holds 16 GiB of memory", lima.id()),
    ] {
        assert!(first.contains(named), "{first:?} does not name {named:?}");
    }
    assert!(end.try_recv().is_err(), "Windows waits");
    stop.store(true, Ordering::SeqCst);
    let fail = end
        .recv_timeout(MUST)
        .expect("the interrupt ends the wait")
        .expect_err("an interrupted wait is NOT RUN");
    let why = fail.msg.clone().unwrap_or_default();
    assert_eq!(
        (fail.code, fail.stopped),
        (crate::linux::NOT_RUN, Some(libc::SIGINT)),
        "{why}"
    );
    for named in [
        "NOT RUN: interrupted while waiting for capacity for aterm-windows",
        "memory short by 2 GiB",
        "this run booted nothing",
    ] {
        assert!(why.contains(named), "{why:?} does not name {named:?}");
    }
    let queued = |ledger: &Ledger| {
        ledger
            .list()
            .expect("listed")
            .into_iter()
            .map(|r| (r.pid, r.credit))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        queued(&ledger),
        [(lima.id(), Vec::new())],
        "the interrupted claim left the queue"
    );

    let mut tell = lima.stdin.take().expect("its stdin");
    writeln!(tell, "{CREDIT}").expect("told to credit");
    let credited = hear(&lima_said, CREDITED);
    assert!(
        credited.contains(&format!("[{}]", vm.id())),
        "the VM's disk is held by the stand-in alone: {credited:?}"
    );
    assert_eq!(queued(&ledger), [(lima.id(), vec![vm.id()])]);
    let deadline = Instant::now() + MUST;
    let mut said = Vec::new();
    let admitted = admit_in(
        &ledger,
        &*machine,
        WINDOWS.need("lab windows up", true),
        &WINDOWS,
        Admit::Wait,
        quick(),
        &mut |l| said.push(l.to_owned()),
        &|| {
            if Instant::now() < deadline {
                Ok(())
            } else {
                Err(crate::stop::Stopped(libc::SIGALRM))
            }
        },
    )
    .unwrap_or_else(|f| panic!("admitted beside the credited claim: {:?}", f.msg));
    assert!(admitted.claim.is_some());
    assert!(
        said.iter().all(|l| !l.contains("waits")),
        "no wait line: {said:?}"
    );

    writeln!(tell, "die").expect("told to die");
    drop(tell);
    lima.wait().expect("the claim stand-in is reaped");
    drop(vm.stdin.take());
    let status = vm.wait().expect("the VM stand-in is reaped");
    assert!(status.success(), "{status}");
}

/// With lima's 16 GiB boot claim held by another process and 30 GiB available on a 48 GiB
/// Mac (short of Windows' 8 GiB and the 8 GiB margin beside lima's 16), Windows' boot
/// waits, its wait line naming lima's claim and its holder, and is admitted the moment room
/// appears, its guest's jobs the cores it was granted. Negative controls: once lima's holder
/// has died by its own hand, the same reading admits Windows at once with no wait line; and
/// a Mac too small for the guest and its margin is NOT RUN at once, nothing waited for.
#[test]
fn a_boot_waits_beside_limas_claim_naming_it_and_boots_when_room_appears() {
    use std::os::unix::process::ExitStatusExt as _;
    let dir = aterm_tempfile::Builder::new()
        .prefix("lab-vm-admit-")
        .tempdir()
        .expect("a scratch ledger");
    let ledger = Ledger::at(dir.path().join("capacity"));
    let mut lima = aterm_uds::spawnfd::command(std::env::current_exe().expect("this test"))
        .args(["--exact", CLAIM_STAND_IN, "--nocapture", "--test-threads=1"])
        .env(CLAIM_ENV, ledger.dir())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("the stand-in starts");
    let lima_pid = lima.id();
    let mut said = BufReader::new(lima.stdout.take().expect("its stdout"));
    let (holds, held) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        while said.read_line(&mut line).is_ok_and(|n| n > 0) {
            if line.contains(HOLDS) {
                let _ = holds.send(());
            }
            line.clear();
        }
    });
    held.recv_timeout(MUST)
        .expect("the stand-in holds lima's claim");

    let machine = Arc::new(Fixed::new(Reading::of_gib(48, 30, 14)));
    let (lines, heard) = std::sync::mpsc::channel::<String>();
    let windows = {
        let (ledger, machine, lines) = (ledger.clone(), Arc::clone(&machine), lines.clone());
        std::thread::spawn(move || {
            admit_in(
                &ledger,
                &*machine,
                WINDOWS.need("lab windows up", true),
                &WINDOWS,
                Admit::Wait,
                quick(),
                &mut |l| {
                    let _ = lines.send(l.to_owned());
                },
                &|| Ok(()),
            )
            .map(|a| (a.jobs, a.claim.map(|c| c.cores())))
            .map_err(|f| f.msg)
        })
    };
    let first = heard.recv_timeout(MUST).expect("Windows' first wait line");
    for named in [
        "lab windows up: aterm-windows boot (8 GiB, 6 vCPUs) waits",
        "memory short by 2 GiB: needs 8 GiB and 8 GiB spare beside it, 30 GiB available \
         less 16 GiB reserved",
        &format!("pid {lima_pid} holds 16 GiB of memory for "),
        "\"lab linux proof: aterm-linux boot (16 GiB, 8 vCPUs)\"",
    ] {
        assert!(first.contains(named), "{first:?} does not name {named:?}");
    }
    assert!(
        !windows.is_finished(),
        "Windows waits while there is no room"
    );
    machine.set(Reading::of_gib(48, 40, 14));
    let (jobs, granted) = windows
        .join()
        .expect("Windows' admission")
        .expect("admitted once room appears");
    assert_eq!(
        (jobs, granted),
        (6, Some(6)),
        "the 14 cores less lima's 8 leave Windows its 6"
    );
    let said: Vec<String> = heard.try_iter().collect();
    assert!(
        said.iter().any(|l| l.contains("admitted after"))
            && said
                .iter()
                .any(|l| l.contains("CARGO_BUILD_JOBS=6: the cores this run was admitted")),
        "{said:?}"
    );

    let mut stdin = lima.stdin.take().expect("its stdin");
    stdin.write_all(b"die\n").expect("told to die");
    let status = lima.wait().expect("the stand-in is reaped");
    assert_eq!(status.signal(), Some(libc::SIGKILL), "{status}");
    machine.set(Reading::of_gib(48, 30, 14));
    let mut said = Vec::new();
    let alone = admit_in(
        &ledger,
        &*machine,
        WINDOWS.need("lab windows up", true),
        &WINDOWS,
        Admit::Wait,
        quick(),
        &mut |l| said.push(l.to_owned()),
        &|| Ok(()),
    )
    .unwrap_or_else(|f| panic!("admitted alone: {:?}", f.msg));
    assert!(alone.claim.is_some());
    assert!(
        said.iter().all(|l| !l.contains("waits")),
        "no wait line: {said:?}"
    );

    let small = Fixed::new(Reading::of_gib(12, 12, 8));
    let mut said = Vec::new();
    let refused = admit_in(
        &ledger,
        &small,
        WINDOWS.need("lab windows up", true),
        &WINDOWS,
        Admit::Wait,
        quick(),
        &mut |l| said.push(l.to_owned()),
        &|| Ok(()),
    )
    .expect_err("a Mac too small is NOT RUN");
    let why = refused.msg.unwrap_or_default();
    assert_eq!(refused.code, crate::linux::NOT_RUN, "{why}");
    assert!(
        why.contains("needs 8 GiB and 8 GiB spare beside it, this Mac has 12 GiB")
            && why.contains("can never boot here"),
        "{why}"
    );
    assert!(said.is_empty(), "nothing waited for: {said:?}");
}

/// NEVER-REACHABLE (`docs/DESIGN-adaptive-build-2026-10-24.md`): on a 28 GiB Mac with 4 GiB
/// wired and 18 GiB available, lima's boot (16 GiB and the 8 GiB margin) is NOT RUN at once,
/// naming the 24 GiB that Mac can ever have available, nothing waited for and nothing left in
/// the ledger; on this Mac's shape (48 GiB, 3.5 GiB wired, 27 GiB available) it is admitted
/// with no wait line. Negative control: judged against all of the memory (the old rule), the
/// small Mac's boot waits for memory it can never have, until the hang detector.
#[test]
fn a_boot_above_what_the_mac_can_ever_have_available_is_not_run_at_once() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("lab-vm-reach-")
        .tempdir()
        .expect("a scratch ledger");
    let ledger = Ledger::at(dir.path().join("capacity"));
    let boot = {
        let ledger = ledger.clone();
        std::thread::spawn(move || {
            let mut said = Vec::new();
            let answer = admit_in(
                &ledger,
                &Fixed::new(Reading::of_gib(28, 18, 14).wired_gib(4)),
                LIMA.need("lab linux exec", true),
                &LIMA,
                Admit::Wait,
                quick(),
                &mut |l| said.push(l.to_owned()),
                &|| Ok(()),
            );
            (answer.map(|a| a.jobs).map_err(|f| (f.code, f.msg)), said)
        })
    };
    let detector = Instant::now() + MUST;
    while !boot.is_finished() {
        assert!(
            Instant::now() < detector,
            "lima's boot on a 28 GiB Mac with 4 GiB wired still waits after {MUST:?}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let (answer, said) = boot.join().expect("the boot's answer");
    let (code, why) = answer.expect_err("NOT RUN");
    let why = why.unwrap_or_default();
    assert_eq!(code, crate::linux::NOT_RUN, "{why}");
    assert!(
        why.contains(
            "lab linux exec: aterm-linux boot (16 GiB, 8 vCPUs): needs 16 GiB and 8 GiB spare \
             beside it, this Mac can never have more than 24 GiB of its 28 GiB of memory \
             available (4 GiB of it is wired); the VM can never boot here"
        ),
        "{why}"
    );
    assert!(said.is_empty(), "nothing waited for: {said:?}");
    assert!(ledger.list().expect("listed").is_empty(), "nothing entered");

    let mac = Reading {
        wired: Some(7 << 29),
        ..Reading::of_gib(48, 27, 14)
    };
    let mut said = Vec::new();
    let admitted = admit_in(
        &ledger,
        &Fixed::new(mac),
        LIMA.need("lab linux exec", true),
        &LIMA,
        Admit::Wait,
        quick(),
        &mut |l| said.push(l.to_owned()),
        &|| Ok(()),
    )
    .unwrap_or_else(|f| panic!("admitted on this Mac's shape: {:?}", f.msg));
    assert!(admitted.claim.is_some());
    assert!(
        said.iter().all(|l| !l.contains("waits")),
        "no wait line: {said:?}"
    );
}

/// The guest's jobs are the cores the claim was granted: on 14 cores at a 5-minute load of
/// 60, lima's boot is granted its share, 3, and its guest builds with `CARGO_BUILD_JOBS=3`
/// (CAP-1's first rule granted 1); a run on the VM already up claims cores alone and is
/// never short of memory.
#[test]
fn the_guests_jobs_are_the_cores_admitted() {
    let dir = aterm_tempfile::tempdir().expect("a scratch ledger");
    let ledger = Ledger::at(dir.path().join("capacity"));
    let mut loaded = Reading::of_gib(48, 48, 14);
    loaded.load5 = Some(60.0);
    let admitted = admit_in(
        &ledger,
        &Fixed::new(loaded),
        LIMA.need("lab linux exec", true),
        &LIMA,
        Admit::Wait,
        quick(),
        &mut |_| {},
        &|| Ok(()),
    )
    .unwrap_or_else(|f| panic!("{:?}", f.msg));
    assert_eq!(admitted.jobs, 3);
    assert_eq!(admitted.claim.as_ref().map(Claim::cores), Some(3));
    let adopted = LIMA.need("lab linux exec", false);
    assert_eq!(
        (adopted.mem, adopted.margin, adopted.cores),
        (0, 0, LIMA.cpus)
    );
    let none = Fixed::new(Reading::of_gib(48, 0, 14));
    let on_up = admit_in(
        &ledger,
        &none,
        adopted,
        &LIMA,
        Admit::Wait,
        quick(),
        &mut |_| {},
        &|| Ok(()),
    )
    .unwrap_or_else(|f| panic!("{:?}", f.msg));
    assert!(on_up.jobs >= 1);
}

/// The booter record's stand-in: as [`BOOTER_STAND_IN`] it records itself as the booter at
/// [`BOOTER_ENV`]'s path, says so, and dies by its own `SIGKILL`.
const BOOTER_STAND_IN: &str = "vm_admit::tests::a_dead_booter_stand_in";
/// Where the booter stand-in records itself.
const BOOTER_ENV: &str = "ATERM_LAB_TEST_VM_BOOTER";

/// Not a test alone: see [`BOOTER_STAND_IN`].
#[test]
fn a_dead_booter_stand_in() {
    let Some(path) = std::env::var_os(BOOTER_ENV) else {
        return;
    };
    Booter::me("lab linux exec", Path::new("/checkouts/booter"))
        .record(Path::new(&path))
        .expect("recorded");
    aterm_uds::process::kill_self_now()
}

/// A VM whose one-shot booter died by its own hand is a leftover, named by its booter's pid,
/// and still is once the record names the VM process its boot started while that process
/// runs; the same record marked kept (`up`, `--keep-up`) is not, nor one whose booter still
/// runs, nor no record at all. Nor is one whose record names a VM process that has ended: that
/// boot is over, and a VM running now was started since outside the records (the negative
/// control for [`Booter::vm_ended`]). A record whose pid now names a process born at another
/// moment (a pid taken again) reads as a dead booter's.
#[test]
fn a_dead_one_shot_booters_vm_is_a_leftover_and_a_kept_one_is_not() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let path = dir.path().join("booter");
    assert_eq!(leftover(&path), None, "no record: adopted");
    let mut child = aterm_uds::spawnfd::command(std::env::current_exe().expect("this test"))
        .args(["--exact", BOOTER_STAND_IN, "--test-threads=1"])
        .env(BOOTER_ENV, &path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("the stand-in starts");
    let pid = child.id();
    let status = child.wait().expect("reaped");
    assert!(!status.success(), "{status}");
    let gone = leftover(&path).expect("a dead one-shot booter's VM is a leftover");
    assert_eq!(gone.pid, pid);
    assert!(
        gone.describe().starts_with(&format!(
            "pid {pid} (lab linux exec, /checkouts/booter, since "
        )),
        "{}",
        gone.describe()
    );
    let fated = |path: &Path| fate(path).map(|(_, f)| f);
    assert_eq!(fated(&path), Some(Fate::Leftover));
    assert!(
        status_line(&path).is_some_and(|l| l.contains(&format!("pid {pid} "))),
        "{:?}",
        status_line(&path)
    );
    let up = Booter {
        vm_pid: Some(std::process::id()),
        vm_birth: birth_of(std::process::id()),
        ..gone.clone()
    };
    up.record(&path).expect("recorded");
    assert_eq!(
        leftover(&path).map(|b| b.pid),
        Some(pid),
        "the VM process its boot started runs on: that boot's leftover"
    );
    let over = Booter {
        vm_pid: Some(pid),
        vm_birth: gone.birth,
        ..gone.clone()
    };
    over.record(&path).expect("recorded");
    assert_eq!(
        fated(&path),
        None,
        "its boot is over: a VM running now is adopted"
    );
    assert_eq!(status_line(&path), None);
    let kept = Booter {
        kept: true,
        ..gone.clone()
    };
    kept.record(&path).expect("recorded");
    assert_eq!(fated(&path), Some(Fate::Kept), "a VM kept up is adopted");
    assert_eq!(leftover(&path), None);
    let me = Booter::me("lab linux proof", Path::new("/checkouts/me"));
    me.record(&path).expect("recorded");
    assert_eq!(fated(&path), Some(Fate::Running), "a live booter's VM");
    assert_eq!(leftover(&path), None);
    let reused = Booter {
        birth: me.birth.map(|(s, us)| (s.saturating_sub(1), us)),
        ..me.clone()
    };
    reused.record(&path).expect("recorded");
    assert_eq!(
        leftover(&path).map(|b| b.pid),
        Some(me.pid),
        "a pid born at another moment is not the booter"
    );
    settle(Some(&path), false, false);
    assert!(
        path.exists(),
        "a VM this run could not stop keeps its record"
    );
    me.record(&path).expect("recorded");
    record_vm(Some(&path), Some(pid));
    settle(Some(&path), true, false);
    assert_eq!(
        Booter::read(&path).map(|b| (b.kept, b.vm_pid)),
        Some((true, Some(pid))),
        "kept, the VM process its boot started still named"
    );
    settle(Some(&path), false, true);
    assert!(!path.exists(), "a stopped VM's record goes");
    assert_eq!(Booter::parse("{\"pid\":1}"), None);
    assert_eq!(Booter::parse(&me.line()), Some(me));
}

/// `--admit now` SHORT OF ROOM ON A COHORT-AWARE LEDGER BOOTS AT ONCE, UNCLAIMED (TM-1
/// review): such a ledger admits only what fits, so its forced look answers
/// [`Error::Refused`] where it once answered a ledger error. With 20 GiB of this Mac's 30
/// available held, lima's boot (16 GiB and the 8 GiB margin) does not fit; under `--admit now`
/// it goes on at once, as [`crate::linux::AT_ONCE`] promises, on every vCPU with no claim, as a
/// ledger it could not use leaves it. NEGATIVE CONTROL (by hand): the refusal ended as NOT RUN
/// fails it; with no arm for it the crate does not build.
#[test]
fn admit_now_short_of_room_on_a_cohort_ledger_boots_at_once_unclaimed() {
    use std::os::unix::fs::DirBuilderExt as _;
    let dir = aterm_tempfile::Builder::new()
        .prefix("lab-vm-cohort-")
        .tempdir()
        .expect("a scratch root");
    let home = dir.path().join("home");
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&home)
        .expect("a private home");
    let namespace = aterm_capacity::cohort_namespace::Namespace::create_private(dir.path(), &home)
        .expect("a cohort-aware ledger");
    let ledger = Ledger::at(namespace.fence());
    assert!(ledger.cohort_aware().expect("its kind"), "cohort-aware");
    let machine = Fixed::new(Reading::of_gib(48, 30, 14));
    let held = match aterm_capacity::Waiter::enter(&ledger, &machine, Need::new("hold").mem_gib(20))
        .expect("the holder entered")
        .poll(&machine)
    {
        aterm_capacity::Polled::Admitted(claim) => claim,
        other => panic!("the holder is not admitted: {other:?}"),
    };
    let admitted = admit_in(
        &ledger,
        &machine,
        LIMA.need("lab linux proof", true),
        &LIMA,
        Admit::Now,
        quick(),
        &mut |_| {},
        &|| Ok(()),
    )
    .unwrap_or_else(|f| panic!("lima boots at once: {:?}", f.msg));
    assert!(admitted.claim.is_none(), "unclaimed");
    assert_eq!(admitted.jobs, LIMA.cpus, "on every vCPU");
    drop(held);
}
