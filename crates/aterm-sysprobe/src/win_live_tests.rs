// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Windows arm read on this host, each reader held to a Windows API it does not call
//! itself, and the capability report held to the readers (design
//! `docs/DESIGN-system-awareness-2026-10-16.md` §16, whose Windows cells these measure).

#![allow(
    non_snake_case,
    clippy::upper_case_acronyms,
    reason = "Win32 names are kept verbatim to check against the SDK"
)]

use std::ffi::c_void;
use std::io::{Read as _, Write as _};
use std::mem::size_of;

use crate::jobs::RunState;
use crate::machine_type::{friendly, is_code, is_placeholder};
use crate::runq::RunQueue;
use crate::windows::{PerformanceInformation64, SessionClass, session_class};
use crate::{LinkReading, ProcessReading};

type BOOL = i32;
type HANDLE = isize;

/// `PROCESS_MEMORY_COUNTERS_EX` (`psapi.h`), 64-bit.
#[repr(C)]
#[derive(Default)]
struct ProcessMemoryCountersEx {
    cb: u32,
    page_fault_count: u32,
    peak_working_set_size: usize,
    working_set_size: usize,
    quota_peak_paged_pool_usage: usize,
    quota_paged_pool_usage: usize,
    quota_peak_non_paged_pool_usage: usize,
    quota_non_paged_pool_usage: usize,
    pagefile_usage: usize,
    peak_pagefile_usage: usize,
    private_usage: usize,
}
const _: () = assert!(size_of::<ProcessMemoryCountersEx>() == 80);

/// `OSVERSIONINFOW` (`winnt.h`).
#[repr(C)]
struct OsVersionInfo {
    size: u32,
    major: u32,
    minor: u32,
    build: u32,
    platform: u32,
    service_pack: [u16; 128],
}
const _: () = assert!(size_of::<OsVersionInfo>() == 276);

/// `MIB_IFROW` (`ifmib.h`), the row of the older `GetIfEntry`, as `DWORD`s: 860 bytes.
const IFROW_WORDS: usize = 215;
/// `MIB_IFROW.dwIndex`, after the 256-unit name.
const IFROW_INDEX: usize = 512 / 4;
/// `MIB_IFROW.dwInOctets`.
const IFROW_IN_OCTETS: usize = 552 / 4;
/// `MIB_IFROW.dwOutOctets`.
const IFROW_OUT_OCTETS: usize = 576 / 4;

#[link(name = "kernel32.dll", kind = "raw-dylib", modifiers = "+verbatim")]
unsafe extern "system" {
    fn K32GetPerformanceInfo(info: *mut PerformanceInformation64, cb: u32) -> BOOL;
    fn ProcessIdToSessionId(pid: u32, session: *mut u32) -> BOOL;
    fn GetCurrentProcess() -> HANDLE;
    fn K32GetProcessMemoryInfo(
        process: HANDLE,
        counters: *mut ProcessMemoryCountersEx,
        cb: u32,
    ) -> BOOL;
}

#[link(name = "ntdll.dll", kind = "raw-dylib", modifiers = "+verbatim")]
unsafe extern "system" {
    fn RtlGetVersion(info: *mut OsVersionInfo) -> i32;
}

#[link(name = "iphlpapi.dll", kind = "raw-dylib", modifiers = "+verbatim")]
unsafe extern "system" {
    fn GetIfEntry(row: *mut c_void) -> u32;
    fn ConvertInterfaceIndexToLuid(index: u32, luid: *mut u64) -> u32;
    fn ConvertInterfaceLuidToAlias(luid: *const u64, alias: *mut u16, length: usize) -> u32;
}

/// `HKEY_LOCAL_MACHINE`: `(LONG)0x80000002`, sign-extended to a handle.
const HKEY_LOCAL_MACHINE: HANDLE = -2_147_483_646;
/// `KEY_QUERY_VALUE` (`winnt.h`): the one right a value read needs.
const KEY_QUERY_VALUE: u32 = 0x0001;
/// `REG_SZ` (`winnt.h`).
const REG_SZ: u32 = 1;
/// `REG_EXPAND_SZ` (`winnt.h`).
const REG_EXPAND_SZ: u32 = 2;

#[link(name = "advapi32.dll", kind = "raw-dylib", modifiers = "+verbatim")]
unsafe extern "system" {
    fn RegOpenKeyExW(
        hKey: HANDLE,
        lpSubKey: *const u16,
        ulOptions: u32,
        samDesired: u32,
        phkResult: *mut HANDLE,
    ) -> i32;
    fn RegQueryValueExW(
        hKey: HANDLE,
        lpValueName: *const u16,
        lpReserved: *mut u32,
        lpType: *mut u32,
        lpData: *mut u8,
        lpcbData: *mut u32,
    ) -> i32;
    fn RegCloseKey(hKey: HANDLE) -> i32;
}

fn performance_info() -> PerformanceInformation64 {
    let cb = u32::try_from(size_of::<PerformanceInformation64>()).expect("a small struct");
    let mut perf = PerformanceInformation64 {
        cb,
        ..PerformanceInformation64::default()
    };
    // SAFETY: `perf` is `cb` writable bytes of PERFORMANCE_INFORMATION.
    assert_ne!(unsafe { K32GetPerformanceInfo(&raw mut perf, cb) }, 0);
    perf
}

/// This process's session, by the API the sweep does not use.
fn session_of_this_process() -> u32 {
    let mut session = u32::MAX;
    // SAFETY: this process's pid and a writable u32.
    assert_ne!(
        unsafe { ProcessIdToSessionId(std::process::id(), &raw mut session) },
        0
    );
    session
}

/// This process's private commit, bytes, by `GetProcessMemoryInfo`.
fn private_usage() -> u64 {
    let cb = u32::try_from(size_of::<ProcessMemoryCountersEx>()).expect("a small struct");
    let mut pmc = ProcessMemoryCountersEx {
        cb,
        ..ProcessMemoryCountersEx::default()
    };
    // SAFETY: the pseudo-handle of this process and `cb` writable bytes.
    assert_ne!(
        unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &raw mut pmc, cb) },
        0
    );
    pmc.private_usage as u64
}

/// The running Windows' build number, by `RtlGetVersion` (no registry read).
fn kernel_build() -> u32 {
    let mut info = OsVersionInfo {
        size: u32::try_from(size_of::<OsVersionInfo>()).expect("a small struct"),
        major: 0,
        minor: 0,
        build: 0,
        platform: 0,
        service_pack: [0; 128],
    };
    // SAFETY: a writable OSVERSIONINFOW with its size set.
    assert_eq!(unsafe { RtlGetVersion(&raw mut info) }, 0);
    info.build
}

/// `(dwInOctets, dwOutOctets)` of interface `index`, by the older `GetIfEntry`: 32-bit counters
/// in a row laid out unlike `MIB_IF_ROW2`. `None` where that API has no such row.
fn old_octets(index: u32) -> Option<(u32, u32)> {
    let mut row = [0u32; IFROW_WORDS];
    row[IFROW_INDEX] = index;
    // SAFETY: `row` is a writable MIB_IFROW (860 bytes) with its index set.
    (unsafe { GetIfEntry(row.as_mut_ptr().cast()) } == 0)
        .then(|| (row[IFROW_IN_OCTETS], row[IFROW_OUT_OCTETS]))
}

/// The alias Windows gives interface `index`, by the LUID conversion calls.
fn alias_of(index: u32) -> Option<String> {
    let mut luid = 0u64;
    // SAFETY: a writable NET_LUID (one u64).
    if unsafe { ConvertInterfaceIndexToLuid(index, &raw mut luid) } != 0 {
        return None;
    }
    let mut buf = [0u16; 257];
    // SAFETY: a readable NET_LUID and `buf.len()` writable wide characters.
    if unsafe { ConvertInterfaceLuidToAlias(&raw const luid, buf.as_mut_ptr(), buf.len()) } != 0 {
        return None;
    }
    let end = buf.iter().position(|u| *u == 0).unwrap_or(buf.len());
    Some(String::from_utf16_lossy(&buf[..end]))
}

/// The firmware's product fields as the registry's BIOS key holds them, by `RegOpenKeyExW` and
/// `RegQueryValueExW` (the machine-type reader calls `RegGetValueW`), kept to the reader's
/// rules: each name with its string, `None` where the key or the value is missing, the value is
/// no string (`REG_SZ`, or `REG_EXPAND_SZ`, which the reader's read takes too), longer than the
/// reader's 255 UTF-16 units, or not valid UTF-16.
fn bios_fields() -> Vec<(&'static str, Option<String>)> {
    const NAMES: [&str; 4] = [
        "SystemManufacturer",
        "SystemProductName",
        "SystemVersion",
        "SystemFamily",
    ];
    let wide = |s: &str| s.encode_utf16().chain(Some(0)).collect::<Vec<u16>>();
    let path = wide(r"HARDWARE\DESCRIPTION\System\BIOS");
    let mut key: HANDLE = 0;
    // SAFETY: a NUL-terminated key path that outlives the call, a predefined root key, and a
    // writable handle.
    let opened = unsafe {
        RegOpenKeyExW(
            HKEY_LOCAL_MACHINE,
            path.as_ptr(),
            0,
            KEY_QUERY_VALUE,
            &raw mut key,
        )
    };
    if opened != 0 {
        // No firmware table copied (no SMBIOS): the reader reads no field either.
        let _ = writeln!(std::io::stderr(), "the BIOS key does not open: {opened}");
        return NAMES.into_iter().map(|name| (name, None)).collect();
    }
    let read = |name: &str| {
        let value = wide(name);
        let mut kind = 0u32;
        let mut buf = [0u16; 256];
        let mut bytes = u32::try_from(size_of_val(&buf)).expect("a small buffer");
        // SAFETY: `key` is open; `value` is NUL-terminated and outlives the call; the reserved
        // pointer is null; `kind` is a writable u32; `buf` is writable for `bytes` bytes, and
        // `bytes` a writable u32 the call sets to the bytes it wrote.
        let status = unsafe {
            RegQueryValueExW(
                key,
                value.as_ptr(),
                std::ptr::null_mut(),
                &raw mut kind,
                buf.as_mut_ptr().cast(),
                &raw mut bytes,
            )
        };
        if status != 0 || !(kind == REG_SZ || kind == REG_EXPAND_SZ) {
            return None;
        }
        let units = &buf[..(bytes as usize / 2).min(buf.len())];
        let len = units.iter().position(|&u| u == 0).unwrap_or(units.len());
        // The reader's buffer holds 255 units and the terminator.
        if len == buf.len() {
            return None;
        }
        String::from_utf16(&units[..len]).ok()
    };
    let fields = NAMES.into_iter().map(|name| (name, read(name))).collect();
    // SAFETY: `key` was opened above and is closed once.
    unsafe { RegCloseKey(key) };
    fields
}

fn own_row() -> ProcessReading {
    let me = std::process::id();
    crate::processes()
        .expect("a sweep")
        .procs
        .into_iter()
        .find(|p| p.pid == me)
        .expect("this process is in the sweep")
}

/// Physical memory is what Windows can use, within 1 % of `GetPerformanceInfo`'s physical
/// total (another call, counted in pages); pressure and the paging figure are read, and the
/// paging figure is no more than the page files hold.
#[test]
fn physical_memory_pressure_and_paging_are_read() {
    let perf = performance_info();
    let oracle_mib = perf.physical_total.saturating_mul(perf.page_size) >> 20;
    let mib = crate::physical_memory_mib().expect("physical memory");
    assert!(
        mib.abs_diff(oracle_mib) * 100 <= oracle_mib,
        "{mib} MiB against GetPerformanceInfo's {oracle_mib} MiB"
    );
    assert!(mib >= 1024, "{mib} MiB");
    let detail = crate::memory_detail().expect("memory detail");
    assert_eq!(detail.total >> 20, mib);
    let level = crate::memory_pressure().expect("a pressure level");
    assert!(
        matches!(level, crate::MemoryLevel::Normal | crate::MemoryLevel::Warn),
        "Windows names no critical level: {level:?}"
    );
    let paged = crate::swapped_kib().expect("the page files' use");
    println!("physical {mib} MiB (oracle {oracle_mib}), pressure {level:?}, paged {paged} KiB");
    // Read after the figure: the page files only grow between the two reads if the machine is
    // paging, and their size is the bound either way.
    let total_kib = crate::memory_detail().expect("memory detail").swap_total / 1024;
    assert!(paged <= total_kib, "{paged} KiB in use of {total_kib} KiB");
}

/// The sweep's row for this process holds its session (as `ProcessIdToSessionId` says it), a
/// working set, a peak commit at least its commit, a running state (this thread is on a core),
/// and transfer counts that rise by at least what this test writes and then reads.
#[test]
fn the_sweeps_own_row_holds_session_memory_state_and_io() {
    let before = own_row();
    assert_eq!(before.uid, Some(session_of_this_process()), "{before:?}");
    assert_eq!(before.uid, crate::own_session(), "{before:?}");
    assert!(before.resident.is_some_and(|b| b > 0), "{before:?}");
    assert!(
        before.peak_footprint >= before.footprint && before.footprint.is_some_and(|b| b > 0),
        "{before:?}"
    );
    assert_eq!(before.state, RunState::Running, "{before:?}");
    assert_eq!((before.sid, before.pgid), (0, 0), "{before:?}");

    const WRITTEN: usize = 4 << 20;
    let dir = aterm_tempfile::tempdir().expect("a scratch directory");
    let path = dir.path().join("written");
    let mut file = std::fs::File::create(&path).expect("a scratch file");
    file.write_all(&vec![0x5A_u8; WRITTEN]).expect("written");
    drop(file);
    let wrote = own_row();
    let grew = |now: Option<u64>, was: Option<u64>| now.zip(was).map(|(n, w)| n.saturating_sub(w));
    assert!(
        grew(wrote.io_written, before.io_written).is_some_and(|b| b >= WRITTEN as u64),
        "{:?} then {:?}",
        before.io_written,
        wrote.io_written
    );
    let mut back = Vec::with_capacity(WRITTEN);
    std::fs::File::open(&path)
        .expect("the scratch file")
        .read_to_end(&mut back)
        .expect("read back");
    assert_eq!(back.len(), WRITTEN);
    let read = own_row();
    assert!(
        grew(read.io_read, wrote.io_read).is_some_and(|b| b >= WRITTEN as u64),
        "{:?} then {:?}",
        wrote.io_read,
        read.io_read
    );
    // The transfer counts hold a pipe's and a console's bytes with a file's, so they are no
    // disk figure: the row names none, and the Disk lens reads no writer from it.
    assert_eq!(
        (read.disk_read, read.disk_written),
        (None, None),
        "{read:?}"
    );
    assert!(read.cpu_ns >= before.cpu_ns);
}

/// Session 0 is the services': the `System` process (pid 4) is always there, and this test's
/// process is this person's. NEGATIVE CONTROL: the two classes differ, so the split is not one
/// class for every row.
#[test]
fn the_system_process_is_a_service_and_this_process_is_this_persons() {
    let sweep = crate::processes().expect("a sweep");
    let own = crate::own_session().expect("this process's session");
    let class = |pid: u32| {
        let row = sweep
            .procs
            .iter()
            .find(|p| p.pid == pid)
            .unwrap_or_else(|| panic!("pid {pid} is in the sweep"));
        session_class(row.uid.expect("a session"), own)
    };
    assert_eq!(class(4), SessionClass::Services);
    // A test run as a service (session 0) is itself a service; anywhere else it is its person's.
    let mine = class(std::process::id());
    if own == 0 {
        assert_eq!(mine, SessionClass::Services);
    } else {
        assert_eq!(mine, SessionClass::Own);
        assert!(
            sweep
                .procs
                .iter()
                .any(|p| p.uid == Some(own) && p.pid != std::process::id()),
            "another process in this session (the one that started the test)"
        );
    }
    let services = sweep.procs.iter().filter(|p| p.uid == Some(0)).count();
    assert!(services >= 2, "{services} session 0 processes");
}

/// A tree's memory is its members' private commit: this process holding 64 MiB reads at least
/// that, near what `GetProcessMemoryInfo` says of it, and a child it starts joins its tree and
/// is a tree of its own. NEGATIVE CONTROLS: a pid that is not running and pid 0 are no tree.
#[test]
fn a_process_trees_memory_is_measured() {
    const HELD_MIB: u64 = 64;
    let held = vec![0xA5_u8; (HELD_MIB as usize) << 20];
    let me = std::process::id();
    let own = crate::group_memory(me).expect("this process's tree");
    let oracle_mib = private_usage() >> 20;
    println!("tree memory {own:?} against GetProcessMemoryInfo's {oracle_mib} MiB");
    assert_eq!(own.pgid, me);
    assert!(own.measured >= 1);
    assert!(own.footprint_mib >= HELD_MIB, "{own:?}");
    // The tree is at least this process; other tests in this binary allocate between the two
    // reads, so the bound is a quarter of the figure plus the held block, not a few pages.
    assert!(
        own.footprint_mib + HELD_MIB + oracle_mib / 4 >= oracle_mib,
        "{own:?} against GetProcessMemoryInfo's {oracle_mib} MiB"
    );
    std::hint::black_box(&held);

    let shell = std::env::var_os("ComSpec").expect("ComSpec names cmd.exe");
    let mut child = aterm_uds::spawnfd::command(shell)
        .args(["/d", "/c", "set /p held="])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("a child that waits on its input");
    let pid = child.id();
    let members = crate::group_usage(me);
    let with_child = crate::group_memory(me);
    let childs = crate::group_memory(pid);
    let childs_usage = crate::group_usage(pid);
    child.kill().expect("the child this test started is ended");
    child.wait().expect("reaped");

    let members = members.expect("this process's tree");
    assert_eq!(members[0].pid, me, "the root first");
    assert!(members[0].start > 0 && members[0].footprint > 0);
    assert!(
        members.iter().any(|m| m.pid == pid),
        "the child {pid} is in {members:?}"
    );
    assert!(with_child.expect("a tree").measured >= 2);
    let childs = childs.expect("the child's own tree");
    assert_eq!(childs.pgid, pid);
    assert!(
        childs.footprint_mib < HELD_MIB,
        "the child's tree does not hold its parent's memory: {childs:?}"
    );
    let childs_usage = childs_usage.expect("the child's own members");
    assert!(childs_usage.iter().all(|m| m.pid != me), "{childs_usage:?}");

    assert_eq!(crate::group_memory(u32::MAX), None);
    assert_eq!(crate::group_memory(0), None);
    assert_eq!(crate::group_usage(u32::MAX), None);
}

/// Live: a process whose parent exited is still measured with the tree it was started in.
/// A shell this test starts runs a second shell that starts a third in the background and
/// exits, which is how Git Bash leaves a script and `cmd /c start` a program: Windows gives the
/// third no new parent. It shares its console with the first, so the tree's memory and its
/// members hold it. NEGATIVE CONTROLS: the parent chain alone, over the same sweep, does not
/// hold it (the defect: an agent's tool calls under Git Bash were never counted), and this
/// test's own console read of a pid that names no process answers nothing.
#[test]
fn a_process_orphaned_in_its_trees_console_is_still_measured() {
    let shell = std::env::var_os("ComSpec").expect("ComSpec names cmd.exe");
    // The first shell waits on its input after the second returns; the third waits on the
    // same input, so closing it ends both.
    let mut child = aterm_uds::spawnfd::command(shell)
        .args([
            "/d",
            "/c",
            "cmd /d /c start /b cmd /d /c set /p held= & set /p held=",
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("a shell that waits on its input");
    let root = child.id();
    // A hang detector, not a budget: the second shell exits as soon as it has started the third.
    let hung = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let (orphan, by_parents, adopted) = loop {
        let sweep = crate::processes().expect("a sweep");
        let live: std::collections::HashSet<u32> = sweep.procs.iter().map(|p| p.pid).collect();
        let members =
            crate::windows::tree_adopting(&sweep.procs, root, |p| crate::win::console_host(p.pid));
        let found = members
            .iter()
            .find(|p| p.pid != root && !live.contains(&p.ppid))
            .map(|p| (p.pid, p.start));
        if let Some(orphan) = found {
            let by_parents: Vec<u32> = crate::windows::tree(&sweep.procs, root)
                .iter()
                .map(|p| p.pid)
                .collect();
            break (
                orphan,
                by_parents,
                members.iter().map(|p| p.pid).collect::<Vec<_>>(),
            );
        }
        assert!(
            std::time::Instant::now() < hung,
            "no orphan joined the tree of {root} within a minute: {:?}",
            members.iter().map(|p| (p.pid, p.ppid)).collect::<Vec<_>>()
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    let memory = crate::group_memory(root);
    let usage = crate::group_usage(root);
    drop(child.stdin.take());
    child
        .wait()
        .expect("the first shell ends at the end of its input");
    // The orphan ends at the same end of input; it is gone before the test is.
    while crate::processes()
        .expect("a sweep")
        .procs
        .iter()
        .any(|p| (p.pid, p.start) == orphan)
    {
        assert!(
            std::time::Instant::now() < hung,
            "the orphan {orphan:?} outlived its input by a minute"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }

    println!("tree of {root}: by parents {by_parents:?}, with its console's orphans {adopted:?}");
    assert!(
        !by_parents.contains(&orphan.0),
        "{orphan:?} names a parent that is gone: {by_parents:?}"
    );
    assert!(by_parents.contains(&root));
    let usage = usage.expect("the shell's tree");
    assert!(
        usage.iter().any(|m| m.pid == orphan.0),
        "the orphan {orphan:?} is in {usage:?}"
    );
    let measured = memory.expect("the shell's tree").measured;
    assert!(
        usize::try_from(measured).is_ok_and(|n| n > by_parents.len()),
        "{measured} measured against {by_parents:?}"
    );
    assert_eq!(crate::win::console_host(u32::MAX - 3), None);
}

/// Live: where this Windows compresses memory, its store is a session 0 row of the sweep
/// under the name the decoder looks for, and naming it as the compressed class leaves the
/// memory used as it was; where it does not (Windows Server), nothing is named. Which of the
/// two this host is, is printed.
#[test]
fn the_compression_store_is_read_from_the_sweep_where_windows_keeps_one() {
    let sweep = crate::processes().expect("a sweep");
    let detail = crate::memory_detail().expect("memory detail");
    let with = crate::windows::with_compression_store(detail, &sweep.procs);
    let store = sweep
        .procs
        .iter()
        .find(|p| p.name == crate::windows::MEMORY_COMPRESSION);
    println!(
        "compression store {:?}: compressed {:?} of {} used",
        store.map(|p| (p.pid, p.uid, p.resident)),
        with.compressed,
        with.used()
    );
    assert_eq!(with.used(), detail.used());
    match store {
        Some(row) => {
            assert_eq!(row.uid, Some(0), "{row:?}");
            assert_eq!(with.compressed, row.resident.map(|b| b.min(detail.app)));
        }
        None => assert_eq!(with, detail),
    }
}

/// The machine's words: a CPU brand, and the OS named with the build `RtlGetVersion` reports
/// (the registry's `ProductName` alone says `Windows 10` on Windows 11).
#[test]
fn the_machine_words_name_the_cpu_and_the_running_windows() {
    let build = kernel_build();
    let words = crate::machine_words();
    println!(
        "machine words {words:?}, build {:?}, model {:?}, kernel build {build}",
        crate::os_build(),
        crate::machine_model()
    );
    let cpu = words.cpu.as_deref().expect("a CPU brand");
    assert!(cpu.len() >= 4 && cpu == cpu.trim(), "{cpu:?}");
    let os = words.os.as_deref().expect("the OS words");
    assert!(os.starts_with("Windows "), "{os:?}");
    assert_eq!(
        os.rsplit(' ').next(),
        Some(build.to_string().as_str()),
        "{os:?} ends with the kernel's build"
    );
    if os.starts_with("Windows 1") {
        let major = if build >= crate::windows::WINDOWS_11_FIRST_BUILD {
            11
        } else {
            10
        };
        assert_eq!(words.os_major(), Some(major), "{os:?} at build {build}");
    }
    let os_build = crate::os_build().expect("the OS build");
    let (number, revision) = os_build.split_once('.').unwrap_or((&os_build, "0"));
    assert_eq!(number, build.to_string(), "{os_build:?}");
    assert!(crate::parse_uint::<u32>(revision).is_some(), "{os_build:?}");
    if let Some(model) = crate::machine_model() {
        assert!(!model.is_empty() && model == model.trim(), "{model:?}");
    }
}

/// `low` lies on the way from `from` to `to`, all three counted modulo 2^32: a 32-bit counter
/// read between two reads of the 64-bit one it is the low half of.
fn between_mod_2_32(from: u64, low: u32, to: u64) -> bool {
    let span = to.wrapping_sub(from);
    let step = low.wrapping_sub(from as u32);
    span <= u64::from(u32::MAX) && u64::from(step) <= span
}

fn by_index(links: &[LinkReading], index: u32) -> Option<&LinkReading> {
    links.iter().find(|l| l.index == index)
}

/// The links are this machine's interfaces: the loopback among them, each named as Windows'
/// own LUID calls name it, no counter running backwards between two reads, and the busiest
/// link's octets agreeing with the older `GetIfEntry` (another row layout, 32-bit counters)
/// read between them.
#[test]
fn the_links_are_this_machines_interfaces_with_their_counters() {
    let first = crate::net_links().expect("the interface table");
    assert!(!first.is_empty());
    let loopbacks: Vec<_> = first.iter().filter(|l| l.loopback).collect();
    assert_eq!(loopbacks.len(), 1, "{loopbacks:?}");
    assert_eq!(loopbacks[0].kind, crate::LinkKind::Other);
    assert!(!crate::counts_toward_machine(loopbacks[0]));
    let mut indexes: Vec<u32> = first.iter().map(|l| l.index).collect();
    indexes.sort_unstable();
    indexes.dedup();
    assert_eq!(
        indexes.len(),
        first.len(),
        "one row an interface: {first:?}"
    );
    for link in &first {
        assert!(!link.name.is_empty(), "{link:?}");
        assert!(link.physical.is_some(), "{link:?}");
        if let Some(alias) = alias_of(link.index).filter(|a| !a.is_empty()) {
            assert_eq!(link.name, alias, "{link:?}");
        }
    }
    let busiest = first
        .iter()
        .filter(|l| l.up && !l.loopback)
        .max_by_key(|l| l.rx.bytes.saturating_add(l.tx.bytes));
    let oracle = busiest.and_then(|l| old_octets(l.index).map(|o| (l.index, o)));
    let second = crate::net_links().expect("the interface table");
    for was in &first {
        let Some(now) = by_index(&second, was.index) else {
            continue;
        };
        for (name, a, b) in [
            ("rx bytes", was.rx.bytes, now.rx.bytes),
            ("rx packets", was.rx.packets, now.rx.packets),
            ("rx errors", was.rx.errors, now.rx.errors),
            ("rx drops", was.rx.drops, now.rx.drops),
            ("tx bytes", was.tx.bytes, now.tx.bytes),
            ("tx packets", was.tx.packets, now.tx.packets),
            ("tx errors", was.tx.errors, now.tx.errors),
            ("tx drops", was.tx.drops, now.tx.drops),
        ] {
            assert!(a <= b, "{name} of {} ran backwards: {a} then {b}", was.name);
        }
    }
    for link in &first {
        println!(
            "link {} {:?} index={} up={} physical={:?} rate={:?} rx={} tx={}",
            link.name,
            link.kind,
            link.index,
            link.up,
            link.physical,
            link.rate_bps,
            link.rx.bytes,
            link.tx.bytes
        );
    }
    println!("GetIfEntry of the busiest up link: {oracle:?}");
    if let Some((index, (rx, tx))) = oracle {
        let was = by_index(&first, index).expect("the busiest link");
        let now = by_index(&second, index).expect("the busiest link, read again");
        assert!(
            was.rx.bytes > 0 || was.tx.bytes > 0,
            "an up link that has carried traffic: {was:?}"
        );
        assert!(
            between_mod_2_32(was.rx.bytes, rx, now.rx.bytes),
            "{}: GetIfEntry's in octets {rx} between {} and {}",
            was.name,
            was.rx.bytes,
            now.rx.bytes
        );
        assert!(
            between_mod_2_32(was.tx.bytes, tx, now.tx.bytes),
            "{}: GetIfEntry's out octets {tx} between {} and {}",
            was.name,
            was.tx.bytes,
            now.tx.bytes
        );
    }
}

/// NEGATIVE CONTROL for the octet check: a counter outside the two reads is refused, and so is
/// a pair of reads more than a 32-bit counter apart.
#[test]
fn a_counter_outside_the_two_reads_is_refused() {
    assert!(between_mod_2_32(100, 150, 200));
    assert!(between_mod_2_32(100, 100, 100));
    assert!(!between_mod_2_32(100, 99, 200));
    assert!(!between_mod_2_32(100, 201, 200));
    assert!(
        between_mod_2_32(0xFFFF_FFF0, 5, 0x1_0000_0010),
        "across a wrap"
    );
    assert!(!between_mod_2_32(0, 5, 0x2_0000_0000));
}

/// The machine's TCP summary counts the connections this test holds open (the kernel completes
/// each handshake into the listener's backlog, so they are established before any accept): at
/// both ends of each, whatever else the machine has open, and the listener among the
/// listening sockets. It says it read every state, from the connection tables, and no
/// round-trip time.
#[test]
fn the_tcp_summary_counts_an_established_connection() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback listener");
    const HELD: u32 = 32;
    let at = listener.local_addr().expect("its address");
    let clients: Vec<_> = (0..HELD)
        .map(|_| std::net::TcpStream::connect(at).expect("connected"))
        .collect();
    let summary = crate::tcp_summary().expect("the TCP statistics");
    println!("tcp summary {summary:?}");
    drop((clients, listener));
    assert!(summary.counts.established >= 2 * HELD, "{summary:?}");
    assert!(summary.counts.listen >= 1, "{summary:?}");
    assert!(!summary.counters_only, "{summary:?}");
    assert!(summary.counts.total() > summary.counts.established);
    assert_eq!(summary.srtt_ms, None);
    assert_eq!(
        crate::tcp_counts().map(|c| c.total() > 0),
        Some(true),
        "the counts alone"
    );
}

/// Whether the reader behind the capability row `name` answers on this host; `None` for a row
/// no reader here stands behind (a new row must name its reader).
fn reader_answers(name: &str) -> Option<bool> {
    let me = std::process::id();
    let system =
        || crate::processes().and_then(|sweep| sweep.procs.into_iter().find(|p| p.pid == 4));
    let memory = crate::memory_detail;
    let table = crate::volumes::table();
    let mounts: Vec<&crate::volumes::Mount> = table.mounts.iter().collect();
    Some(match name {
        "per_core" => crate::Probe::new().core_ticks().is_some(),
        "core_kinds" | "cores_withdrawn" => crate::withdrawn_cores().is_some(),
        "memory_classes" | "swap" => memory().is_some(),
        "pressure" => crate::memory_pressure().is_some(),
        "commit" => memory().is_some_and(|m| m.commit.is_some()),
        "compressor" => memory().zip(crate::processes()).is_some_and(|(m, s)| {
            crate::windows::with_compression_store(m, &s.procs)
                .compressed
                .is_some()
        }),
        "gpu_device" => crate::gpu().is_some(),
        "gpu_per_proc" => crate::gpu_clients().is_some(),
        "proc_cpu_others" => system().is_some_and(|p| p.cpu_ns.is_some()),
        "proc_memory_others" => system().is_some_and(|p| p.footprint.is_some()),
        "throttle" => crate::Probe::new().throttle().is_ok(),
        "volume" => crate::volume(&std::env::temp_dir()).is_some(),
        "coalitions" => crate::coalitions::self_check(me).is_ok(),
        "physical_memory" => crate::physical_memory_mib().is_some(),
        "paging" => crate::swapped_kib().is_some(),
        "proc_owner" => system().is_some_and(|p| p.uid.is_some()) && crate::own_session().is_some(),
        "proc_io" => system().is_some_and(|p| p.io_read.is_some() && p.io_written.is_some()),
        "proc_state" => system().is_some_and(|p| p.state != RunState::Unknown),
        "tree_memory" => crate::group_memory(me).is_some() && crate::group_usage(me).is_some(),
        "machine_words" => {
            let words = crate::machine_words();
            words.cpu.is_some() && words.os.is_some()
        }
        "net_links" => crate::net_links().is_some(),
        "tcp_established" => crate::tcp_summary().is_some(),
        "tcp_states" => crate::tcp_summary().is_some_and(|s| !s.counters_only),
        "proc_sockets" => crate::open_socket_count(me).is_some(),
        "device_io" => !crate::blockio::BlockReader::new().read(&mounts).is_empty(),
        "load" => {
            crate::load_average().is_some() || crate::task_room(0).run_queue.threads().is_some()
        }
        _ => return None,
    })
}

/// THE ACCEPTANCE CHECK of the Windows arm: the capability report says of each reader what
/// that reader does on this host. A row that says `yes` or `estimate` has a reader that
/// answers, a row that says `unavailable` has one that does not, and every row names a reader
/// (the table is printed, so a row that fell to `unavailable` fails by its name).
#[test]
fn the_capability_report_says_what_each_reader_does_here() {
    let caps = crate::caps();
    let rows = caps.rows();
    let table: String = rows
        .iter()
        .map(|(name, word)| format!("\n  {name}={word}"))
        .collect();
    println!("this host's capability report:{table}");
    assert_eq!(rows.first(), Some(&("os", "windows".to_owned())), "{table}");
    assert_eq!(
        rows.last(),
        Some(&("capacity", "host".to_owned())),
        "{table}"
    );
    let mut names: Vec<&str> = rows.iter().map(|(name, _)| *name).collect();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), rows.len(), "a row named twice:{table}");
    for (name, word) in &rows[1..rows.len() - 1] {
        let says = !word.starts_with("unavailable(");
        let answers = reader_answers(name)
            .unwrap_or_else(|| panic!("the row {name} names no reader this test knows:{table}"));
        assert_eq!(
            says, answers,
            "the report says {name}={word} and its reader answers {answers}:{table}"
        );
    }
    // What this build is known to read on any Windows 10 or 11: a row here that turns
    // unavailable is a reader that stopped answering, by name.
    for name in [
        "per_core",
        "memory_classes",
        "pressure",
        "commit",
        "swap",
        "proc_cpu_others",
        "proc_memory_others",
        "volume",
        "physical_memory",
        "paging",
        "proc_owner",
        "proc_io",
        "proc_state",
        "tree_memory",
        "machine_words",
        "net_links",
        "tcp_established",
        "tcp_states",
    ] {
        let word = rows
            .iter()
            .find(|(row, _)| *row == name)
            .map(|(_, word)| word.as_str());
        assert!(
            matches!(word, Some("yes" | "estimate")),
            "{name} reads {word:?}:{table}"
        );
    }
}

/// NEGATIVE CONTROL for the acceptance check: a row with no reader behind it is refused, so a
/// new row cannot join the report unheld.
#[test]
fn a_capability_row_without_a_reader_is_refused() {
    assert_eq!(reader_answers("a_row_nobody_reads"), None);
    assert_eq!(reader_answers("os"), None);
    assert_eq!(
        reader_answers("load"),
        Some(false),
        "Windows has no load average"
    );
}

/// Live: the GPU on this host, read ten times a window apart. A machine DirectX lists a
/// hardware adapter for reads it by name, every read with a share at most 100 (the first too:
/// it takes its own two collections), its memory in use, and the same adapter throughout or a
/// listed one; a machine with none (a VM on the Basic Render Driver) reads no GPU. Each read's
/// wall time is printed (`--nocapture`): the first opens the query, the rest are one collection.
#[test]
fn the_gpu_reads_on_this_host_with_a_share_and_a_name() {
    use std::time::Instant;
    let hardware: Vec<String> = crate::win_pdh::adapters()
        .into_iter()
        .filter(|a| a.vendor != crate::win_gpu::VENDOR_MICROSOFT)
        .map(|a| a.name)
        .collect();
    let started = Instant::now();
    let first = crate::gpu();
    let opened = started.elapsed();
    println!("gpu first read {opened:?}: {first:?} (DirectX lists {hardware:?})");
    let Some(first) = first else {
        // No reading: the counters name no adapter DirectX lists as hardware now (the rows
        // may be an earlier boot's), and then the clients read wherever the device does not.
        return;
    };
    assert!(first.device_pct.is_some_and(|p| p <= 100), "{first:?}");
    assert_eq!(first.unread, None, "{first:?}");
    assert!(first.in_use_bytes.is_some(), "{first:?}");
    if !hardware.is_empty() {
        let name = first.name.as_deref().expect("a listed adapter's name");
        assert!(hardware.iter().any(|h| h.trim() == name), "{first:?}");
    }
    let mut costs = Vec::new();
    for _ in 0..10 {
        std::thread::sleep(crate::win_gpu::MIN_WINDOW + std::time::Duration::from_millis(10));
        let at = Instant::now();
        let read = crate::gpu().expect("the GPU read a moment ago");
        costs.push(at.elapsed());
        assert!(read.device_pct.is_some_and(|p| p <= 100), "{read:?}");
        assert!(
            read.alloc_bytes.is_none_or(|b| b > 0) && read.in_use_bytes.is_some(),
            "{read:?}"
        );
    }
    costs.sort_unstable();
    println!(
        "gpu collection cost over {} reads: median {:?}, max {:?}",
        costs.len(),
        costs[costs.len() / 2],
        costs[costs.len() - 1]
    );
    let driver = crate::gpu_driver();
    println!("gpu driver {driver:?}");
    if let Some(version) = driver.and_then(|d| d.version) {
        assert_eq!(version.split('.').count(), 4, "{version}");
    }
}

/// Live: the GPU's clients on this host. Every engine instance is listed once, the read is
/// complete, no row is pid 0's, a row whose process the sweep lists carries that process's
/// name, and this very process (which holds no device) is charged no GPU time. A machine where
/// no process holds a device reads no row, which is no failure.
#[test]
fn the_gpu_clients_read_every_engine_instance_once() {
    let Some(clients) = crate::gpu_clients() else {
        assert_eq!(crate::gpu(), None, "clients read wherever the device does");
        return;
    };
    let at = std::time::Instant::now();
    let again = crate::gpu_clients().expect("read a moment ago");
    println!(
        "gpu clients: {} rows of {} instances, complete={}; a read inside the window costs {:?}",
        clients.rows.len(),
        clients.children,
        clients.complete,
        at.elapsed()
    );
    assert!(clients.complete && !clients.truncated, "{clients:?}");
    assert_eq!(clients.entries.len(), clients.children as usize);
    let mut ids = clients.entries.clone();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), clients.entries.len(), "an instance listed twice");
    let sweep = crate::processes().expect("the process sweep");
    let mut top: Vec<(u64, u32, &str)> = Vec::new();
    for row in &clients.rows {
        assert!(row.pid != 0, "{row:?}");
        assert!(clients.entries.contains(&row.entry), "{row:?}");
        assert!(row.pid != std::process::id() || row.gpu_ns == 0, "{row:?}");
        if let Some(p) = sweep.procs.iter().find(|p| p.pid == row.pid)
            && !row.name.is_empty()
        {
            assert_eq!(row.name, p.name, "{row:?}");
        }
        top.push((row.gpu_ns, row.pid, &row.name));
    }
    top.sort_unstable_by(|a, b| b.cmp(a));
    for (ns, pid, name) in top.iter().take(5) {
        println!("  pid={pid} name={name} gpu_s={:.1}", *ns as f64 / 1e9);
    }
    // The counters only grow while an instance lives: a second read never reads less.
    let before: std::collections::HashMap<u64, u64> =
        clients.rows.iter().map(|r| (r.entry, r.gpu_ns)).collect();
    for row in &again.rows {
        if let Some(ns) = before.get(&row.entry) {
            assert!(row.gpu_ns >= *ns, "{row:?} read {ns} before");
        }
    }
}

/// Live: the throttle level on this host. A machine with an ACPI thermal zone reads a level
/// that is never called exact; one without reads absent, never "none".
#[test]
fn the_throttle_level_reads_or_says_no_zone() {
    let mut probe = crate::Probe::new();
    let at = std::time::Instant::now();
    let first = probe.throttle();
    let opened = at.elapsed();
    let at = std::time::Instant::now();
    let second = probe.throttle();
    println!(
        "throttle {first:?} (first read {opened:?}), then {second:?} ({:?})",
        at.elapsed()
    );
    match first {
        Ok(t) => assert!(!t.exact, "{t:?}"),
        Err(why) => assert_eq!(why, crate::Why::Absent),
    }
    assert_eq!(first.is_ok(), second.is_ok());
}

/// Live: the job probe's reading of this very process counts its I/O (`sys jobs`' stuck engine
/// has more than CPU time to see work by). A megabyte written through a file raises the logical
/// and the disk writes by at least that and the calls by at least one, held to a write whose
/// size this test chose; a pid that names no process reads nothing.
#[test]
fn the_job_probe_counts_this_process_io() {
    use crate::jobs::{JobsReader, ReadReq};
    const WROTE: u64 = 1 << 20;
    let me = ReadReq {
        pid: std::process::id(),
        fds: false,
    };
    let mut reader = JobsReader::new();
    let before = reader.read(me).expect("this process");
    let before = before.counters.expect("its counters");
    let dir = aterm_tempfile::tempdir().expect("a scratch directory");
    let mut file = std::fs::File::create(dir.path().join("wrote")).expect("a scratch file");
    file.write_all(&vec![7u8; WROTE as usize])
        .expect("a megabyte written");
    drop(file);
    let after = reader.read(me).expect("this process");
    println!("job probe of this process: {after:?}");
    assert_eq!(
        after.name,
        crate::windows::short_image_name(after.exe.clone().expect("its image"))
    );
    assert!(after.start.is_some() && after.age_ms.is_some(), "{after:?}");
    let after = after.counters.expect("its counters");
    let rose = |now: Option<u64>, then: Option<u64>| {
        now.expect("read")
            .checked_sub(then.expect("read"))
            .expect("a counter never falls")
    };
    assert!(
        rose(after.logical_writes, before.logical_writes) >= WROTE,
        "{before:?} then {after:?}"
    );
    assert!(
        rose(after.disk_written, before.disk_written) >= WROTE,
        "{before:?} then {after:?}"
    );
    assert!(
        rose(after.syscalls, before.syscalls) >= 1,
        "{before:?} then {after:?}"
    );
    let _ = rose(after.disk_read, before.disk_read);
    assert!(after.cpu_ns >= before.cpu_ns);
    // NEGATIVE CONTROL: pid 0 (the idle process) is no job, and reads nothing.
    assert!(reader.read(ReadReq { pid: 0, fds: false }).is_none());
}

/// Live, N2 (SD-03): Windows has no run-queue reader, so the read that counts the machine's
/// processes and threads answers the run queue `Unsupported`, never a count and never a zero.
#[test]
fn n2_windows_live_the_run_queue_reads_unsupported() {
    let room = crate::task_room(0);
    // Past libtest's capture, so a passing run's log keeps what this machine answered.
    let _ = writeln!(std::io::stderr(), "task room {room:?}");
    assert!(
        room.threads.is_some_and(|n| n > 0) && room.procs.is_some_and(|n| n > 0),
        "the same read counted the processes and threads: {room:?}"
    );
    assert_eq!(room.run_queue, RunQueue::Unsupported, "{room:?}");
}

/// Live, N2 (SD-03, M-NAME's VM case): the machine type the registry's BIOS key gives reads a
/// name or no name, never a code or a placeholder. The fields are read again here by calls the
/// reader does not make ([`bios_fields`]); what names the machine is the first of the product
/// name, version and family that can name one, and the reader's name is that field with at
/// most the vendor's word before it. The identifier is the product name as written, unless it
/// is a placeholder.
#[test]
fn n2_windows_live_the_machine_type_is_a_name_or_none() {
    let ty = crate::machine_type::read().expect("Windows has a machine-type reader");
    let fields = bios_fields();
    let _ = writeln!(
        std::io::stderr(),
        "machine type {ty:?}; BIOS key {fields:?}"
    );
    let field = |name: &str| {
        fields
            .iter()
            .find(|(n, _)| *n == name)
            .and_then(|(_, v)| v.as_deref())
    };
    let expected = ["SystemProductName", "SystemVersion", "SystemFamily"]
        .into_iter()
        .find_map(|name| field(name).and_then(friendly));
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
                "no NUL or newline from the registry: {name:?}"
            );
            assert!(
                !is_code(name) && !is_placeholder(name),
                "not a code or a placeholder: {name:?}"
            );
        }
        (None, None) => {}
        (name, field) => panic!(
            "the reader's name {name:?} and the field that names the machine {field:?} disagree"
        ),
    }
    let product = field("SystemProductName").map(str::trim);
    match ty.model_id.as_deref() {
        Some(id) => {
            assert_eq!(
                Some(id),
                product,
                "the identifier is the product name as written"
            );
            assert!(!is_placeholder(id), "{id:?}");
        }
        None => assert!(
            product.is_none_or(is_placeholder),
            "a product name {product:?} that is no placeholder is the identifier"
        ),
    }
}
