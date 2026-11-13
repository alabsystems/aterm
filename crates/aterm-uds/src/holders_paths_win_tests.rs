// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::win::{
    Namer, disk_device_itself, helper_errors_quiet, stuck_for, win32_normalized,
    with_spelling_stuck,
};
use super::{Exe, PathBuf, exe_of, scan_path_users, scan_path_users_of};
use aterm_sysprobe::volumes::{Mount, NETWORK, Table};
use std::path::Path;
use std::time::{Duration, Instant};

const HANG: Duration = Duration::from_secs(60);

fn scan_of(pids: &[u32], dirs: &[PathBuf], skip: &[u32]) -> std::io::Result<super::PathScan> {
    super::scan_path_users_of_within(pids, dirs, skip, HANG)
}

fn two_drives() -> Table {
    let volume = |on: &str, n: u32, guid: &str| Mount {
        on: PathBuf::from(on),
        from: format!(r"\Device\HarddiskVolume{n}"),
        fstype: String::new(),
        local: true,
        automounted: false,
        removable: false,
        read_only: false,
        container: Some(format!("Volume{{{guid}}}")),
    };
    Table {
        mounts: vec![
            Mount {
                on: PathBuf::from(NETWORK),
                from: "UNC".to_owned(),
                fstype: String::new(),
                local: false,
                automounted: false,
                removable: false,
                read_only: false,
                container: None,
            },
            volume(r"C:\", 3, "6140fc3b-044f-43d7-b060-cb98170c7a63"),
            volume(r"D:\", 4, "0a1b2c3d-4e5f-4a6b-8c7d-9e0f1a2b3c4d"),
        ],
        complete: true,
        firmlinks: Vec::new(),
    }
}

fn placed(path: &Path, dirs: &[PathBuf]) -> Option<bool> {
    super::win::placed(path, dirs, &two_drives(), &mut Namer::within(HANG)).under
}

fn placed_here(path: &Path, dirs: &[PathBuf]) -> Option<bool> {
    placement_here(path, dirs).under
}

fn placement_here(path: &Path, dirs: &[PathBuf]) -> super::win::Placed {
    let table = aterm_sysprobe::volumes::table();
    super::win::placed(path, dirs, &table, &mut Namer::within(HANG))
}

const PARK_ENV: &str = "ATERM_UDS_WIN_PARK";
const PARK_HOLD: &str = "ATERM_UDS_WIN_HOLD";
const PARK_LOAD: &str = "ATERM_UDS_WIN_LOAD";
const PARK_WATCH: &str = "ATERM_UDS_WIN_WATCH";
const PARK_READY: &str = "ATERM_UDS_WIN_READY";
const PARK_DEVICE: &str = "ATERM_UDS_WIN_DEVICE";
const PARK_DENY: &str = "ATERM_UDS_WIN_DENY";

#[derive(Default)]
struct Park<'a> {
    hold: Option<&'a Path>,
    load: Option<&'a Path>,
    watch: Option<&'a Path>,
    ready: Option<&'a Path>,
    device: Option<&'a Path>,
    deny: bool,
}

#[test]
fn park_mode_sleeps() {
    use std::os::windows::ffi::OsStrExt as _;
    use std::os::windows::fs::OpenOptionsExt as _;
    use std::os::windows::io::AsRawHandle as _;
    const DONT_RESOLVE_DLL_REFERENCES: u32 = 0x0000_0001;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    const NOTIFY_ALL: u32 = 0x0000_001F;
    if std::env::var_os(PARK_ENV).is_none() {
        return;
    }
    if std::env::var_os(PARK_DENY).is_some() {
        const SDDL_REVISION: u32 = 1;
        const DACL: u32 = 0x0000_0004;
        let sddl: Vec<u16> = "D:(D;;0x50;;;WD)(A;;0x101000;;;WD)"
            .encode_utf16()
            .chain([0])
            .collect();
        let mut sd: *mut core::ffi::c_void = std::ptr::null_mut();
        // SAFETY: `sddl` is NUL-terminated and `sd` receives a system-allocated security descriptor.
        let made = unsafe {
            crate::win::ffi::ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION,
                &mut sd,
                std::ptr::null_mut(),
            )
        };
        assert_ne!(
            made,
            0,
            "build the DACL: {}",
            std::io::Error::last_os_error()
        );
        // SAFETY: `GetCurrentProcess()` is the current-process pseudo-handle and `sd` is valid.
        let set = unsafe {
            crate::win::ffi::SetKernelObjectSecurity(crate::win::ffi::GetCurrentProcess(), DACL, sd)
        };
        assert_ne!(set, 0, "set the DACL: {}", std::io::Error::last_os_error());
    }
    let _held = std::env::var_os(PARK_HOLD).map(|path| {
        std::fs::File::open(&path).unwrap_or_else(|e| panic!("open {path:?} to hold: {e}"))
    });
    let _device = std::env::var_os(PARK_DEVICE).map(|path| {
        std::fs::OpenOptions::new()
            .access_mode(0)
            .open(&path)
            .unwrap_or_else(|e| panic!("open {path:?} with no access: {e}"))
    });
    if let Some(module) = std::env::var_os(PARK_LOAD) {
        let wide: Vec<u16> = module.encode_wide().chain([0]).collect();
        // SAFETY: `wide` is a NUL-terminated path loaded as a data file for the process lifetime.
        let mapped = unsafe {
            crate::win::ffi::LoadLibraryExW(wide.as_ptr(), 0, DONT_RESOLVE_DLL_REFERENCES)
        };
        assert_ne!(
            mapped,
            0,
            "map {module:?}: {}",
            std::io::Error::last_os_error()
        );
    }
    let watched = std::env::var_os(PARK_WATCH).map(|dir| {
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(&dir)
            .unwrap_or_else(|e| panic!("open {dir:?} to watch: {e}"))
    });
    if let Some(ready) = std::env::var_os(PARK_READY) {
        std::fs::write(&ready, b"ready").expect("say the child is set up");
    }
    if let Some(dir) = &watched {
        let mut buf = [0u32; 1024];
        let mut got: u32 = 0;
        // SAFETY: `dir` is a synchronous directory handle and `buf` is 4096 aligned writable bytes.
        let _ = unsafe {
            crate::win::ffi::ReadDirectoryChangesW(
                dir.as_raw_handle() as crate::win::ffi::RawHandle,
                buf.as_mut_ptr().cast(),
                4096,
                1,
                NOTIFY_ALL,
                &mut got,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
    }
    std::thread::sleep(Duration::from_secs(120));
}

fn fresh(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("aterm-uds-winpaths-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("mkdir");
    std::fs::canonicalize(&d).expect("canonical")
}

fn spawn_parked(cwd: &Path, hold: Option<&Path>) -> Parked {
    spawn_parked_with(
        cwd,
        &Park {
            hold,
            ..Park::default()
        },
    )
}

struct Parked(crate::spawnfd::Child);
impl std::ops::Deref for Parked {
    type Target = crate::spawnfd::Child;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl std::ops::DerefMut for Parked {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}
impl Drop for Parked {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn spawn_parked_with(cwd: &Path, park: &Park<'_>) -> Parked {
    const NAME: &str = "holders::paths::win_tests::park_mode_sleeps";
    let exe = std::env::current_exe().expect("the test binary");
    let mut cmd = crate::spawnfd::command(exe);
    cmd.args(["--exact", NAME, "--nocapture", "--test-threads=1"])
        .env(PARK_ENV, "1")
        .current_dir(cwd)
        .stdin(crate::spawnfd::Stdio::null())
        .stdout(crate::spawnfd::Stdio::null())
        .stderr(crate::spawnfd::Stdio::null());
    for (var, path) in [
        (PARK_HOLD, park.hold),
        (PARK_LOAD, park.load),
        (PARK_WATCH, park.watch),
        (PARK_READY, park.ready),
        (PARK_DEVICE, park.device),
    ] {
        if let Some(path) = path {
            cmd.env(var, path);
        }
    }
    if park.deny {
        cmd.env(PARK_DENY, "1");
    }
    Parked(cmd.spawn().expect("spawn a parked child"))
}

fn wait_ready(child: &mut crate::spawnfd::Child, ready: &Path) {
    let start = Instant::now();
    while !ready.exists() {
        assert!(
            child.try_wait().expect("try_wait").is_none(),
            "the child exited before it was set up"
        );
        assert!(start.elapsed() < HANG, "the child never said it was set up");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn until(
    dirs: &[PathBuf],
    child: &mut crate::spawnfd::Child,
    want: impl Fn(Option<&str>) -> bool,
) -> Option<String> {
    let pid = child.id();
    let start = Instant::now();
    loop {
        let scan = scan_of(&[pid], dirs, &[std::process::id()]).expect("scan");
        let how = scan
            .users
            .iter()
            .find(|u| u.pid == pid)
            .map(|u| u.how.to_owned());
        if want(how.as_deref()) {
            return how;
        }
        assert!(
            child.try_wait().expect("try_wait").is_none(),
            "the child exited before it was read"
        );
        assert!(start.elapsed() < HANG, "never read as wanted");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn for_cmd(path: &Path) -> String {
    let s = path.to_str().expect("a test path is UTF-8");
    s.strip_prefix(r"\\?\").unwrap_or(s).to_owned()
}

#[test]
fn a_child_working_under_the_directory_uses_it() {
    let dir = fresh("cwd");
    let deep = dir.join("target").join("debug").join("deps");
    std::fs::create_dir_all(&deep).expect("deps");
    let mut worker = spawn_parked(&deep, None);
    let how = until(std::slice::from_ref(&dir), &mut worker, |h| h.is_some());

    let empty = fresh("nobody");
    let me = std::process::id();
    let scan = scan_of(&[worker.id(), me], std::slice::from_ref(&empty), &[me]).expect("scan");
    let _ = worker.kill();
    let _ = worker.wait();
    assert_eq!(how.as_deref(), Some("cwd"));
    assert!(scan.users.is_empty(), "{:?}", scan.users);
    assert!(scan.unreadable.is_empty(), "{:?}", scan.unreadable);
    assert_eq!(
        scan.scanned, 1,
        "the child alone is read; this process is skipped"
    );
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&empty);
}

#[test]
fn a_child_started_in_a_short_spelling_uses_its_folder_and_no_other() {
    let dir = fresh("short");
    let folder = dir.join("a folder with a long name");
    std::fs::create_dir(&folder).expect("the long-named folder");
    let short = aterm_sysprobe::short_spelling(&folder).unwrap_or_else(|| {
        panic!(
            "PREMISE: the volume generates 8.3 short names; none for {}",
            folder.display()
        )
    });
    let other = fresh("short-other");
    let ready = dir.join("short.ready");
    let park = Park {
        ready: Some(&ready),
        ..Park::default()
    };
    let mut child = spawn_parked_with(Path::new(&for_cmd(&short)), &park);
    wait_ready(&mut child, &ready);
    let pid = child.id();
    let reported = aterm_sysprobe::process_cwd(pid).expect("its working directory");
    let table = aterm_sysprobe::volumes::table();
    assert!(
        aterm_sysprobe::paths::short_spelled(&reported).is_some(),
        "{reported:?} is short"
    );
    assert_eq!(table.is_under(&reported, &folder), None, "{reported:?}");

    let mine = scan_of(&[pid], std::slice::from_ref(&folder), &[]).expect("scan");
    let how = mine.users.iter().find(|u| u.pid == pid).map(|u| u.how);
    assert_eq!(how, Some("cwd"), "{:?}", mine.users);
    let theirs = scan_of(&[pid], std::slice::from_ref(&other), &[]).expect("scan");
    assert!(theirs.users.is_empty(), "{:?}", theirs.users);
    assert!(theirs.unreadable.is_empty(), "{:?}", theirs.unreadable);
    let both = scan_of(&[pid], &[folder.clone(), other.clone()], &[]).expect("scan");
    drop(child);
    let found = both.users.iter().find(|u| u.pid == pid).expect("a user");
    assert_eq!(found.how, "cwd", "{found:?}");
    assert_eq!(
        table.is_under(&found.path, &folder),
        Some(true),
        "{found:?}"
    );
    assert_eq!(
        table.is_under(&found.path, &other),
        Some(false),
        "{found:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&other);
}

#[test]
fn a_child_working_through_a_short_spelled_junction_uses_the_directory_it_is_in() {
    let cand = fresh("short-junction");
    let target = fresh("short-junction-target");
    std::fs::create_dir(target.join("sub")).expect("the target's sub");
    let link = cand.join("a junction with a long name");
    junction(&target, &link);
    let short = aterm_sysprobe::short_spelling(&link.join("sub")).unwrap_or_else(|| {
        panic!(
            "PREMISE: the volume generates 8.3 short names; none for {}",
            link.display()
        )
    });
    let ready = target.join("junction.ready");
    let park = Park {
        ready: Some(&ready),
        ..Park::default()
    };
    let mut child = spawn_parked_with(Path::new(&for_cmd(&short)), &park);
    wait_ready(&mut child, &ready);
    let pid = child.id();
    let reported = aterm_sysprobe::process_cwd(pid).expect("its working directory");
    assert!(
        aterm_sysprobe::paths::short_spelled(&reported).is_some(),
        "{reported:?} is short"
    );

    let mine = scan_of(&[pid], std::slice::from_ref(&cand), &[]).expect("scan");
    let theirs = scan_of(&[pid], std::slice::from_ref(&target), &[]).expect("scan");
    drop(child);
    let how = |scan: &super::PathScan| scan.users.iter().find(|u| u.pid == pid).map(|u| u.how);
    assert_eq!(how(&mine), Some("cwd"), "{mine:?}");
    assert_eq!(how(&theirs), Some("fd"), "{theirs:?}");
    std::fs::remove_dir(&link).expect("the junction, as itself");
    let _ = std::fs::remove_dir_all(&cand);
    let _ = std::fs::remove_dir_all(&target);
}

#[test]
fn a_short_spelling_is_spelled_long_on_the_naming_helper() {
    let dir = fresh("short-helper");
    let folder = dir.join("a folder with a long name");
    std::fs::create_dir(&folder).expect("the long-named folder");
    let short = aterm_sysprobe::short_spelling(&folder).unwrap_or_else(|| {
        panic!(
            "PREMISE: the volume generates 8.3 short names; none for {}",
            folder.display()
        )
    });
    let path = short.join("not-made-yet");
    let normalized = win32_normalized(&path);
    let through = aterm_sysprobe::paths::short_spelled(&normalized)
        .expect("short-spelled")
        .through;
    let dirs = [folder.clone()];
    let held = with_spelling_stuck(&through, || placed_here(&path, &dirs));
    assert_eq!(held, None, "a stuck expansion holds {path:?}");
    let spelled = placement_here(&path, &dirs);
    assert_eq!(spelled.under, Some(true), "{path:?}");
    let key = aterm_sysprobe::paths::windows_key;
    assert_eq!(
        key(&spelled.at),
        key(&folder.join("not-made-yet")),
        "{:?}",
        spelled.at
    );
    assert!(key(&spelled.at).is_some(), "{:?}", spelled.at);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_process_unreadable_once_is_read_again_and_held_only_if_it_fails_again() {
    use super::win::Read;
    let at = |pid: u32| super::PathUse {
        pid,
        how: "cwd",
        path: PathBuf::from(format!(r"C:\d\{pid}")),
    };
    let exe = |pid: u32| Some(PathBuf::from(format!(r"C:\bin\p{pid}.exe")));
    let mut asked: Vec<Vec<u32>> = Vec::new();
    let scan = super::settled(&[1, 2, 3, 4, 5, 6, 7], &[7], |pids| {
        let n = asked.len();
        asked.push(pids.to_vec());
        let read = |pid| match (n, pid) {
            (0, 1) => Read::User(at(1)),
            (0, 2) => Read::None,
            (0, _) => Read::Unreadable(exe(pid)),
            (1, 3) => Read::None,
            (1, 4) => Read::Exited,
            (1, 5) => Read::User(at(5)),
            (1, 6) => Read::Unreadable(None),
            _ => panic!("pass {n} read {pid}"),
        };
        pids.iter().map(|&pid| (pid, read(pid))).collect()
    });
    assert_eq!(asked, [vec![1, 2, 3, 4, 5, 6], vec![3, 4, 5, 6]]);
    assert_eq!(scan.users, [at(1), at(5)]);
    assert_eq!(scan.unreadable, [(6, exe(6))]);
    assert_eq!(scan.scanned, 6);

    let mut passes = 0;
    let clean = super::settled(&[1, 2], &[], |pids| {
        passes += 1;
        pids.iter().map(|&pid| (pid, Read::None)).collect()
    });
    assert_eq!((passes, clean.unreadable.len(), clean.scanned), (1, 0, 2));
}

#[test]
fn a_process_that_refuses_the_open_is_held_without_a_second_read() {
    use super::win::Read;
    let exe = |pid: u32| Some(PathBuf::from(format!(r"C:\Windows\System32\p{pid}.exe")));
    let mut asked: Vec<Vec<u32>> = Vec::new();
    let scan = super::settled(&[1, 2, 3], &[], |pids| {
        asked.push(pids.to_vec());
        let read = |pid| match pid {
            1 => Read::None,
            _ => Read::Refused(exe(pid)),
        };
        pids.iter().map(|&pid| (pid, read(pid))).collect()
    });
    assert_eq!(asked, [vec![1, 2, 3]], "refusals alone make one pass");
    assert_eq!(scan.unreadable, [(2, exe(2)), (3, exe(3))]);
    assert_eq!(scan.scanned, 3);

    let mut asked: Vec<Vec<u32>> = Vec::new();
    let scan = super::settled(&[1, 2, 3], &[], |pids| {
        let n = asked.len();
        asked.push(pids.to_vec());
        let read = |pid| match (n, pid) {
            (0, 1) => Read::Unreadable(exe(1)),
            (0, 2) => Read::Refused(None),
            (0, 3) | (1, 1) => Read::None,
            _ => panic!("pass {n} read {pid}"),
        };
        pids.iter().map(|&pid| (pid, read(pid))).collect()
    });
    assert_eq!(asked, [vec![1, 2, 3], vec![1]], "the failed read alone");
    assert_eq!(scan.unreadable, [(2, None)]);
    assert!(scan.users.is_empty(), "{:?}", scan.users);
}

#[test]
fn a_handle_closed_since_the_snapshot_holds_nothing() {
    use super::win::Read;
    let kind = |read: &Read| match read {
        Read::User(u) => format!("user {}", u.how),
        Read::None => "none".to_owned(),
        Read::Exited => "exited".to_owned(),
        Read::Unreadable(exe) => format!("unreadable {exe:?}"),
        Read::Refused(exe) => format!("refused {exe:?}"),
    };
    let dir = fresh("closed-handle");
    let elsewhere = fresh("closed-handle-elsewhere");
    let ready = dir.join("closed.ready");
    let park = Park {
        ready: Some(&ready),
        ..Park::default()
    };
    let mut child = spawn_parked_with(&dir, &park);
    wait_ready(&mut child, &ready);
    let pid = child.id();
    let table = aterm_sysprobe::volumes::table();
    let mut handles = super::win::file_handles()
        .and_then(|mut all| all.remove(&pid))
        .expect("the child's handles in a system snapshot");
    handles.push(0x00FF_FFFC);
    let mut namer = Namer::within(HANG);
    let mut read = |dirs: &[PathBuf]| {
        kind(&super::win::uses(
            pid,
            dirs,
            &table,
            Some(&handles),
            &mut namer,
        ))
    };
    let free = read(std::slice::from_ref(&elsewhere));
    let used = read(std::slice::from_ref(&dir));
    drop(child);
    assert_eq!(free, "none");
    assert_eq!(used, "user cwd");
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&elsewhere);
}

#[test]
fn a_child_holding_a_file_under_the_directory_uses_it() {
    let dir = fresh("fd");
    let deep = dir.join("target").join("debug").join("deps");
    std::fs::create_dir_all(&deep).expect("deps");
    let file = deep.join("build-output.log");
    std::fs::write(&file, b"x").expect("a file to hold");
    let mut holder = spawn_parked(&std::env::temp_dir(), Some(&file));
    let how = until(std::slice::from_ref(&dir), &mut holder, |h| h == Some("fd"));
    let _ = holder.kill();
    let _ = holder.wait();
    assert_eq!(how.as_deref(), Some("fd"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_held_file_whose_path_cannot_be_placed_keeps_its_live_holder() {
    use std::os::windows::ffi::OsStringExt as _;
    let dir = fresh("unplaced");
    let deep = dir.join("target").join("debug");
    std::fs::create_dir_all(&deep).expect("debug");
    let name: Vec<u16> = [u16::from(b'o'), 0xD800]
        .into_iter()
        .chain(".log".encode_utf16())
        .collect();
    let unplaceable = deep.join(std::ffi::OsString::from_wide(&name));
    std::fs::write(&unplaceable, b"x").expect("a lone-surrogate name is a legal NTFS name");
    let elsewhere = fresh("unplaced-control");
    let plain = elsewhere.join("plain.log");
    std::fs::write(&plain, b"x").expect("a plain file");

    let read = |hold: &Path, tag: &str| {
        let ready = elsewhere.join(format!("{tag}.ready"));
        let park = Park {
            hold: Some(hold),
            ready: Some(&ready),
            ..Park::default()
        };
        let mut child = spawn_parked_with(&std::env::temp_dir(), &park);
        wait_ready(&mut child, &ready);
        let pid = child.id();
        let scan =
            scan_of(&[pid], std::slice::from_ref(&dir), &[std::process::id()]).expect("scan");
        let alive = child.try_wait().expect("try_wait").is_none();
        let _ = child.kill();
        let _ = child.wait();
        assert!(alive, "the child must still run when it is read");
        (
            scan.users.iter().any(|u| u.pid == pid),
            scan.unreadable.iter().find(|(p, _)| *p == pid).cloned(),
        )
    };

    let (user, unreadable) = read(&unplaceable, "unplaced");
    assert!(
        user || unreadable.is_some(),
        "a live holder of an unplaceable file under the directory read as a non-user"
    );
    if let Some((_, image)) = unreadable {
        assert!(image.is_some(), "a held process is named by its image");
    }
    let (user, unreadable) = read(&plain, "control");
    assert!(!user, "the control holds nothing under the directory");
    assert_eq!(unreadable, None, "the control child must read cleanly");
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&elsewhere);
}

#[test]
fn a_module_mapped_from_under_the_directory_is_a_user() {
    let dir = fresh("module");
    let deps = dir.join("target").join("debug").join("deps");
    std::fs::create_dir_all(&deps).expect("deps");
    let system = std::env::var_os("SystemRoot").expect("SystemRoot");
    let module = deps.join("probe_macro.dll");
    std::fs::copy(
        Path::new(&system).join("System32").join("version.dll"),
        &module,
    )
    .expect("copy a system DLL under the directory");
    let ready = dir.join("module.ready");
    let park = Park {
        load: Some(&module),
        ready: Some(&ready),
        ..Park::default()
    };
    let mut child = spawn_parked_with(&std::env::temp_dir(), &park);
    wait_ready(&mut child, &ready);
    let pid = child.id();
    let scan = scan_of(&[pid], std::slice::from_ref(&deps), &[std::process::id()]).expect("scan");
    let _ = child.kill();
    let _ = child.wait();
    let found = scan.users.iter().find(|u| u.pid == pid);
    assert_eq!(found.map(|u| u.how), Some("exe"), "{scan:?}");
    let name = found
        .and_then(|u| u.path.file_name())
        .and_then(|n| n.to_str());
    assert_eq!(
        name.map(str::to_ascii_lowercase).as_deref(),
        Some("probe_macro.dll"),
        "the module, not the image, is the witness"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

fn junction(target: &Path, link: &Path) {
    crate::host::could_not_run_under_redirection_guard(
        "the test makes a junction as this user and goes through it",
    );
    let status = crate::spawnfd::command("cmd")
        .args(["/C", "mklink", "/J", &for_cmd(link), &for_cmd(target)])
        .stdin(crate::spawnfd::Stdio::null())
        .stdout(crate::spawnfd::Stdio::null())
        .stderr(crate::spawnfd::Stdio::null())
        .status()
        .expect("run mklink");
    assert!(status.success(), "mklink /J {link:?} {target:?}");
}

#[test]
fn a_module_mapped_through_a_junction_from_under_the_directory_is_a_user() {
    let dir = fresh("module-link");
    let real = dir.join("real");
    let deps = real.join("target").join("debug").join("deps");
    std::fs::create_dir_all(&deps).expect("deps");
    let system = std::env::var_os("SystemRoot").expect("SystemRoot");
    std::fs::copy(
        Path::new(&system).join("System32").join("version.dll"),
        deps.join("probe_linked.dll"),
    )
    .expect("copy a system DLL under the directory");
    let link = dir.join("link");
    junction(&real, &link);
    let through = Path::new(&for_cmd(&link))
        .join("target")
        .join("debug")
        .join("deps")
        .join("probe_linked.dll");
    let target = std::fs::canonicalize(real.join("target")).expect("canonical");
    assert_eq!(
        placed_here(&through, std::slice::from_ref(&target)),
        Some(false),
        "the loader's spelling alone places outside the directory"
    );
    let elsewhere = fresh("module-link-control");
    let ready = elsewhere.join("module.ready");
    let park = Park {
        load: Some(&through),
        ready: Some(&ready),
        ..Park::default()
    };
    let mut child = spawn_parked_with(&std::env::temp_dir(), &park);
    wait_ready(&mut child, &ready);
    let pid = child.id();
    let scan = scan_of(&[pid], std::slice::from_ref(&target), &[std::process::id()]).expect("scan");
    let control = scan_of(
        &[pid],
        std::slice::from_ref(&elsewhere),
        &[std::process::id()],
    )
    .expect("scan");
    let alive = child.try_wait().expect("try_wait").is_none();
    let _ = child.kill();
    let _ = child.wait();
    assert!(alive, "the child must still run when it is read");
    let found = scan.users.iter().find(|u| u.pid == pid);
    assert_eq!(found.map(|u| u.how), Some("exe"), "{scan:?}");
    let name = found
        .and_then(|u| u.path.file_name())
        .and_then(|n| n.to_str());
    assert_eq!(
        name.map(str::to_ascii_lowercase).as_deref(),
        Some("probe_linked.dll"),
        "the module, not the image, is the witness"
    );
    assert!(control.users.is_empty(), "{control:?}");
    assert!(
        control.unreadable.is_empty(),
        "every mapped module of a plain child is placed: {control:?}"
    );
    let _ = std::fs::remove_dir(&link);
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&elsewhere);
}

#[test]
fn a_mapped_files_kernel_name_is_spelled_through_its_volumes_mount_point() {
    use super::win::on_volume;
    let mut table = two_drives();
    table.mounts.push(Mount {
        on: PathBuf::from(r"C:\mnt\e"),
        from: r"\Device\HarddiskVolume5".to_owned(),
        fstype: String::new(),
        local: true,
        automounted: false,
        removable: false,
        read_only: false,
        container: Some("Volume{11111111-2222-4333-8444-555555555555}".to_owned()),
    });
    table.mounts.push(Mount {
        on: PathBuf::from(r"S:\"),
        from: r"\Device\HarddiskVolume6".to_owned(),
        fstype: aterm_sysprobe::volumes::SUBST.to_owned(),
        local: true,
        automounted: false,
        removable: false,
        read_only: false,
        container: None,
    });
    let on = |kernel: &str| on_volume(Path::new(kernel), &table);
    assert_eq!(
        on(r"\Device\HarddiskVolume3\real\target\debug\deps\x.dll"),
        Some(PathBuf::from(r"\\?\C:\real\target\debug\deps\x.dll"))
    );
    assert_eq!(
        on(r"\device\harddiskvolume4\a"),
        Some(PathBuf::from(r"\\?\D:\a"))
    );
    assert_eq!(
        on(r"\Device\HarddiskVolume5\x\y.dll"),
        Some(PathBuf::from(r"\\?\C:\mnt\e\x\y.dll"))
    );
    assert_eq!(
        placed(
            &on(r"\Device\HarddiskVolume3\Real\Target\x.dll").expect("spelled"),
            &[PathBuf::from(r"\\?\C:\real\target")]
        ),
        Some(true)
    );
    for kernel in [
        r"\Device\Mup\srv\share\x.dll",
        r"\Device\HarddiskVolume9\x.dll",
        r"\Device\HarddiskVolume6\x.dll",
        r"C:\real\x.dll",
        r"\Device\",
    ] {
        assert_eq!(on(kernel), None, "{kernel}");
    }
}

#[test]
fn a_running_image_under_the_directory_is_a_user() {
    let dir = fresh("image");
    let comspec = std::env::var_os("ComSpec").expect("ComSpec");
    let runner = dir.join("runner.exe");
    std::fs::copy(&comspec, &runner).expect("copy cmd.exe into the directory");
    let mut child = crate::spawnfd::command(&runner)
        .arg("/k")
        .current_dir(std::env::temp_dir())
        .stdin(crate::spawnfd::Stdio::piped())
        .stdout(crate::spawnfd::Stdio::null())
        .stderr(crate::spawnfd::Stdio::null())
        .spawn()
        .expect("run the copied image");
    let how = until(std::slice::from_ref(&dir), &mut child, |h| h.is_some());
    drop(child.stdin.take());
    let _ = child.kill();
    let _ = child.wait();
    assert_eq!(how.as_deref(), Some("exe"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_git_bash_child_working_under_the_directory_uses_it() {
    let sleep = Path::new(r"C:\Program Files\Git\usr\bin\sleep.exe");
    if !sleep.exists() {
        println!(
            "SKIP holders::paths::win_tests::\
             a_git_bash_child_working_under_the_directory_uses_it: \
             Git for Windows is not installed ({} absent)",
            sleep.display()
        );
        return;
    }
    let dir = fresh("msys");
    let deep = dir.join("deep");
    std::fs::create_dir_all(&deep).expect("deep");
    let mut child = crate::spawnfd::command(sleep)
        .arg("120")
        .current_dir(&deep)
        .stdin(crate::spawnfd::Stdio::null())
        .stdout(crate::spawnfd::Stdio::null())
        .stderr(crate::spawnfd::Stdio::null())
        .spawn()
        .expect("spawn Git Bash sleep");
    let how = until(std::slice::from_ref(&dir), &mut child, |h| h == Some("cwd"));
    let _ = child.kill();
    let _ = child.wait();
    assert_eq!(how.as_deref(), Some("cwd"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_32_bit_child_that_moved_under_the_directory_uses_it() {
    use std::io::Write as _;
    let system = std::env::var_os("SystemRoot").expect("SystemRoot");
    let cmd32 = Path::new(&system).join("SysWOW64").join("cmd.exe");
    if !cmd32.exists() {
        println!(
            "SKIP holders::paths::win_tests::\
             a_32_bit_child_that_moved_under_the_directory_uses_it: \
             no 32-bit subsystem ({} absent)",
            cmd32.display()
        );
        return;
    }
    let dir = fresh("wow64");
    let deep = dir.join("target").join("debug");
    std::fs::create_dir_all(&deep).expect("debug");
    let mut child = crate::spawnfd::command(&cmd32)
        .args(["/q", "/k"])
        .current_dir(std::env::temp_dir())
        .stdin(crate::spawnfd::Stdio::piped())
        .stdout(crate::spawnfd::Stdio::null())
        .stderr(crate::spawnfd::Stdio::null())
        .spawn()
        .expect("run the 32-bit cmd");
    let mut stdin = child.stdin.take().expect("its stdin");
    write!(stdin, "cd /d \"{}\"\r\n", for_cmd(&deep)).expect("ask it to change directory");
    stdin.flush().expect("flush");
    let how = until(std::slice::from_ref(&dir), &mut child, |h| h == Some("cwd"));
    drop(stdin);
    let _ = child.kill();
    let _ = child.wait();
    assert_eq!(how.as_deref(), Some("cwd"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_child_blocked_in_a_synchronous_watch_is_held_and_the_scan_returns() {
    let dir = fresh("watch-scan");
    let watched = fresh("watched");
    let ready = watched.parent().expect("temp").join(format!(
        "aterm-uds-winpaths-watch-{}.ready",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&ready);
    let park = Park {
        watch: Some(&watched),
        ready: Some(&ready),
        ..Park::default()
    };
    let mut child = spawn_parked_with(&std::env::temp_dir(), &park);
    wait_ready(&mut child, &ready);
    let pid = child.id();
    let held = |child: &mut crate::spawnfd::Child| {
        let (tx, rx) = std::sync::mpsc::channel();
        let dirs = vec![dir.clone()];
        let me = std::process::id();
        std::thread::spawn(move || {
            let _ = tx.send(scan_path_users_of(&[pid], &dirs, &[me]));
        });
        let scan = rx
            .recv_timeout(HANG)
            .expect("the scan returns while a child blocks in a watch")
            .expect("scan");
        assert!(
            child.try_wait().expect("try_wait").is_none(),
            "the child exited before it was read"
        );
        assert!(scan.users.is_empty(), "{:?}", scan.users);
        scan.unreadable.iter().any(|(p, _)| *p == pid)
    };
    let start = Instant::now();
    while !held(&mut child) {
        assert!(
            start.elapsed() < HANG,
            "the blocked watch was never read as held"
        );
    }
    assert!(
        stuck_for(pid) > 0,
        "the query that did not answer is remembered"
    );
    assert!(
        held(&mut child),
        "the next scan holds it without asking again"
    );

    let _ = child.kill();
    let _ = child.wait();
    let start = Instant::now();
    while stuck_for(pid) > 0 {
        assert!(
            start.elapsed() < HANG,
            "the abandoned helper never returned once the child ended"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = std::fs::remove_file(&ready);
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&watched);
}

#[test]
fn exe_of_names_a_live_image_and_an_exited_one_as_exited() {
    let mut child = spawn_parked(&std::env::temp_dir(), None);
    let pid = child.id();
    let own = std::fs::canonicalize(std::env::current_exe().expect("the test binary"))
        .expect("canonical");
    let named = exe_of(pid);
    let _ = child.kill();
    let _ = child.wait();
    match named {
        Exe::At(image) => assert_eq!(placed_here(&image, &[own]), Some(true), "{image:?}"),
        other => panic!("a live child's image is named, not {other:?}"),
    }
    assert_eq!(exe_of(pid), Exe::Exited, "a waited-for child still held");
}

#[test]
fn verbatim_plain_and_differently_cased_spellings_all_place_under() {
    let dir = vec![PathBuf::from(r"\\?\C:\Users\x\Proj")];
    for spelling in [
        r"\\?\C:\Users\x\Proj\target\debug",
        r"\??\C:\Users\x\Proj\target\debug", // the NT spelling
        r"C:\Users\x\Proj\target\debug",
        r"c:\users\X\PROJ\target\debug",
        r"C:\Users\x\Proj\target\debug\", // a trailing separator
        r"C:\Users\x\Proj",               // the directory itself
        r"C:\Users\x\Proj\",
        r"C:/Users//x/Proj/target", // `/` separates outside a verbatim prefix
    ] {
        assert_eq!(placed(Path::new(spelling), &dir), Some(true), "{spelling}");
    }
    for spelling in [
        r"C:\Users\x\Project\target", // a sibling sharing a prefix
        r"C:\Users\x\ProjOther",
        r"D:\Users\x\Proj\target", // another volume
        r"C:\",
    ] {
        assert_eq!(placed(Path::new(spelling), &dir), Some(false), "{spelling}");
    }
    assert_eq!(placed(Path::new(r"\\?\C:/Users//x/Proj"), &dir), None);
    let root = vec![PathBuf::from(r"\\?\C:\")];
    assert_eq!(placed(Path::new(r"c:\x\y"), &root), Some(true));

    let share = vec![PathBuf::from(r"\\?\UNC\srv\share\Proj")];
    for spelling in [
        r"\\srv\share\Proj\t",
        r"\\?\UNC\SRV\Share\proj",
        r"\\srv\share\Proj",
    ] {
        assert_eq!(
            placed(Path::new(spelling), &share),
            Some(true),
            "{spelling}"
        );
    }
    for spelling in [
        r"\\srv\share\Projx",
        r"\\srv2\share\Proj",
        r"\\srv\other\Proj",
        r"C:\Users\x\Proj",
    ] {
        assert_eq!(placed(Path::new(spelling), &share), None, "{spelling}");
    }
    assert_eq!(placed(Path::new(r"\\srv\share\Proj\t"), &dir), None);
    assert_eq!(placed(Path::new(r"S:\Users\x\Proj\target"), &dir), None);
    let cut = Table {
        complete: false,
        ..two_drives()
    };
    let mut namer = Namer::default();
    let under = super::win::placed(Path::new(r"c:\USERS\x\proj\t"), &dir, &cut, &mut namer);
    assert_eq!(under.under, Some(true));
    let other = super::win::placed(Path::new(r"D:\Users\x\Proj\t"), &dir, &cut, &mut namer);
    assert_eq!(other.under, None);
}

#[test]
fn a_path_outside_a_drive_or_share_root_cannot_be_placed() {
    use std::os::windows::ffi::OsStringExt as _;
    let dir = vec![PathBuf::from(r"\\?\C:\Users\x\Proj")];
    for spelling in [
        r"\\.\C:\Users\x\Proj\t",
        r"\\?\Volume{0a1b2c3d-0000-0000-0000-100000000000}\Users\x\Proj",
        r"\\?\GLOBALROOT\Device\HarddiskVolume3\Users\x\Proj",
        r"\Device\HarddiskVolume3\Users\x\Proj",
        r"Users\x\Proj\t",
        r"C:Users\x\Proj",
        r"\Users\x\Proj",
        r"\\server",
        r"\\?\C:\Users\x\Proj\..\..\y",
        r"\\?\C:\Users\x\.\Proj",
        r"\\?\C:\Users\x\\Proj\t",
        r"C:\Users\x\Proj.\t",
        r"C:\Users\x\Proj \t",
    ] {
        assert_eq!(placed(Path::new(spelling), &dir), None, "{spelling}");
    }
    let empty = fresh("short-none");
    let ghost = empty.join("NOSUCH~1").join("t");
    assert_eq!(
        placed(&ghost, std::slice::from_ref(&empty)),
        None,
        "{ghost:?}"
    );
    let _ = std::fs::remove_dir_all(&empty);
    assert_eq!(
        placed(Path::new(r"\\?\C:\Users\x\Proj.\t"), &dir),
        Some(false)
    );
    let lone: Vec<u16> = r"\\?\C:\Users\x\P".encode_utf16().chain([0xD800]).collect();
    let unkeyable = PathBuf::from(std::ffi::OsString::from_wide(&lone));
    let path = Path::new(r"C:\Users\x\Proj\t");
    assert_eq!(placed(path, std::slice::from_ref(&unkeyable)), None);
    assert_eq!(placed(path, &[unkeyable, dir[0].clone()]), Some(true));
}

#[test]
fn a_win32_path_places_as_windows_normalizes_it() {
    let dir = vec![PathBuf::from(r"\\?\C:\Users\x\Proj")];
    for spelling in [
        r"C:\Users\x\Other\..\Proj\t",
        r"C:\Users\x\.\Proj",
        r"C:\Users\x\\Proj\t",
        r"C:\..\..\Users\x\Proj",
        r"\\?\C:\Users\x\Proj\t",
    ] {
        assert_eq!(placed(Path::new(spelling), &dir), Some(true), "{spelling}");
    }
    for spelling in [r"C:\Users\x\Proj\..\Other", r"C:\Users\x\Proj\t\..\..\.."] {
        assert_eq!(placed(Path::new(spelling), &dir), Some(false), "{spelling}");
    }
    let bash = Path::new(r"C:\Program Files\Git\bin\..\usr\bin\bash.exe");
    let usr = vec![PathBuf::from(r"\\?\C:\Program Files\Git\usr")];
    let bin = vec![PathBuf::from(r"\\?\C:\Program Files\Git\bin")];
    assert_eq!(placed(bash, &usr), Some(true));
    assert_eq!(placed(bash, &bin), Some(false));
    let share = vec![PathBuf::from(r"\\?\UNC\srv\share\Proj")];
    assert_eq!(
        placed(Path::new(r"\\srv\share\x\..\Proj\t"), &share),
        Some(true)
    );
    assert_eq!(
        placed(Path::new(r"\\srv\share\..\Proj"), &share),
        Some(true)
    );
    assert_eq!(
        placed(Path::new(r"\\srv\share\..\other\Proj"), &share),
        None
    );
    for (from, to) in [
        (r"C:\a\.\b\..\c", r"C:\a\c"),
        (r"C:\..", r"C:\"),
        (r"c:/x//y/", r"c:\x\y"),
        (r"\\srv\share\..\..\x", r"\\srv\share\x"),
        (r"\\?\C:\a\..\b", r"\\?\C:\a\..\b"),
        (r"\\.\C:\a\..\b", r"\\.\C:\a\..\b"),
        (r"C:a\..\b", r"C:a\..\b"),
    ] {
        assert_eq!(win32_normalized(Path::new(from)), Path::new(to), "{from}");
    }
}

#[test]
fn the_naming_helper_holds_the_error_boxes_off() {
    assert_eq!(helper_errors_quiet(HANG), Some(true), "the helper's thread");
    assert!(
        !aterm_sysprobe::errors_quiet(),
        "a thread with no guard has them on"
    );
}

#[test]
fn only_a_disk_device_opened_itself_is_no_file() {
    for name in [
        r"\Device\HarddiskVolume3",
        r"\Device\Harddisk0\DR0",
        r"\Device\Harddisk12\Partition2",
        r"\Device\CdRom0",
    ] {
        assert!(disk_device_itself(name), "{name}");
    }
    for name in [
        r"\Device\HarddiskVolume3\Users\x",
        r"\Device\HarddiskVolume3\",
        r"\Device\HarddiskVolumeShadowCopy1",
        r"\Device\HarddiskVolume",
        r"\Device\Harddisk0\DR0\x",
        r"\Device\Harddisk0",
        r"\Device\Mup\srv\share",
        r"\Device\NamedPipe\x",
        r"\Device\Null",
        r"\??\C:",
        "",
    ] {
        assert!(!disk_device_itself(name), "{name}");
    }
}

#[test]
fn a_child_that_refuses_the_reads_is_held_and_named() {
    let dir = fresh("refused");
    let ready = dir.join("refused.ready");
    let park = Park {
        ready: Some(&ready),
        deny: true,
        ..Park::default()
    };
    let mut child = spawn_parked_with(&dir, &park);
    wait_ready(&mut child, &ready);
    let pid = child.id();
    let scan = scan_of(&[pid], std::slice::from_ref(&dir), &[std::process::id()]).expect("scan");
    let named = exe_of(pid);
    let alive = child.try_wait().expect("try_wait").is_none();
    let _ = child.kill();
    let _ = child.wait();
    assert!(alive, "the child must still run when it is read");
    let own = std::fs::canonicalize(std::env::current_exe().expect("the test binary"))
        .expect("canonical");
    assert!(scan.users.is_empty(), "{:?}", scan.users);
    match scan.unreadable.iter().find(|(p, _)| *p == pid) {
        Some((_, Some(image))) => {
            assert_eq!(
                placed_here(image, std::slice::from_ref(&own)),
                Some(true),
                "{image:?}"
            );
        }
        other => panic!("a refusing live child is held and named, not {other:?}"),
    }
    match named {
        Exe::At(image) => assert_eq!(placed_here(&image, &[own]), Some(true), "{image:?}"),
        other => panic!("its image is named, not {other:?}"),
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_child_holding_a_volume_open_reads_cleanly() {
    let dir = fresh("volume");
    let ready = dir.join("volume.ready");
    let drive = std::env::var("SystemDrive").expect("SystemDrive");
    let volume = PathBuf::from(format!(r"\\.\{drive}"));
    let park = Park {
        device: Some(&volume),
        ready: Some(&ready),
        ..Park::default()
    };
    let mut child = spawn_parked_with(&std::env::temp_dir(), &park);
    wait_ready(&mut child, &ready);
    let pid = child.id();
    let empty = dir.join("empty");
    std::fs::create_dir_all(&empty).expect("empty");
    let scan = scan_of(&[pid], std::slice::from_ref(&empty), &[std::process::id()]).expect("scan");
    let alive = child.try_wait().expect("try_wait").is_none();
    let _ = child.kill();
    let _ = child.wait();
    assert!(alive, "the child must still run when it is read");
    assert!(scan.users.is_empty(), "{:?}", scan.users);
    assert!(scan.unreadable.is_empty(), "{:?}", scan.unreadable);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn case_folds_one_unit_to_one() {
    let strasse = vec![PathBuf::from(r"\\?\C:\Straße")];
    assert_eq!(placed(Path::new(r"C:\STRAßE\x"), &strasse), Some(true));
    assert_eq!(placed(Path::new(r"c:\straße\x"), &strasse), Some(true));
    assert_eq!(placed(Path::new(r"C:\STRASSE\x"), &strasse), Some(false));
    let umlaut = vec![PathBuf::from(r"\\?\C:\Ärger")];
    assert_eq!(placed(Path::new(r"c:\ärger\x"), &umlaut), Some(true));
    let greek = vec![PathBuf::from("\\\\?\\C:\\\u{1FB3}")];
    assert_eq!(placed(Path::new("C:\\\u{1FBC}\\x"), &greek), Some(true));
    let deseret = vec![PathBuf::from("\\\\?\\C:\\\u{10428}")];
    assert_eq!(placed(Path::new("C:\\\u{10400}\\x"), &deseret), Some(false));
}

#[test]
#[ignore = "a measurement, run by hand"]
fn the_scan_times_itself_and_names_unreadable_processes() {
    let empty = fresh("measure");
    let start = Instant::now();
    let scan = scan_path_users(std::slice::from_ref(&empty), &[std::process::id()])
        .expect("the full scan");
    let elapsed = start.elapsed();
    println!(
        "MEASURED holders::paths scan: {} same-user processes in {:.1} ms, {} unreadable",
        scan.scanned,
        elapsed.as_secs_f64() * 1000.0,
        scan.unreadable.len()
    );
    for (pid, exe) in &scan.unreadable {
        println!(
            "  unreadable pid {pid}: {}",
            exe.as_ref()
                .map_or("image unnamed".to_owned(), |e| e.display().to_string())
        );
    }
    assert!(
        scan.scanned > 0,
        "some process of this account besides this one (skipped, so not counted) is scanned"
    );
    let _ = std::fs::remove_dir_all(&empty);
}
