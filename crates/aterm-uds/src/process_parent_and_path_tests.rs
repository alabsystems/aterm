// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{read_process_parent, read_process_path};

#[test]
fn this_process_reads_its_parent_and_its_executable() {
    let me = i32::try_from(std::process::id()).expect("pid");
    assert_eq!(
        read_process_parent(me),
        Some(std::os::unix::process::parent_id())
    );
    let path = read_process_path(me).expect("own path");
    let exe = std::env::current_exe().expect("current_exe");
    assert_eq!(
        path.canonicalize().expect("path exists"),
        exe.canonicalize().expect("exe exists")
    );
}

/// A child reads as the program it runs, translated or not: under a binary translator the
/// kernel names the translator, and the read takes the program the kernel handed it
/// (`AT_EXECFD`). A translated child reads `None` from its exec until its translator has
/// mapped its program, so the first read is waited for. It is not run under a translator the
/// kernel hands no program it maps executable (`translator_holds_its_program`), where a
/// translated process reads as its translator. NEGATIVE CONTROL, under a translator: once
/// the child holds its program, which it opened itself, on that descriptor in place of the
/// kernel's exec open, it reads `None`, to this process and to itself alike: a file a
/// process holds open is no proof it runs it, mapped or not, and Rosetta's own answer to a
/// translated process's path read of its `exe` is no record of the kernel's.
#[test]
fn a_child_reads_as_the_program_it_runs() {
    use std::io::Write as _;
    use std::path::{Path, PathBuf};
    const NAME: &str = "process::parent_and_path_tests::a_child_reads_as_the_program_it_runs";
    const PARK_ENV: &str = "ATERM_UDS_TEST_PATH_PARK";
    const ITSELF: &str = "it reads itself as None";
    if std::env::var_os(PARK_ENV).is_some() {
        let mut line = String::new();
        let _ = std::io::stdin().read_line(&mut line);
        #[cfg(target_os = "linux")]
        if let Ok(fd) = line.trim().parse::<i32>() {
            use std::os::fd::AsRawFd as _;
            let own = std::fs::File::open(std::env::current_exe().expect("the program"))
                .expect("the program, opened");
            // SAFETY: `own` is open for the call; `dup2` closes what `fd` held and puts a
            // copy of `own` there, and nothing in this process owns `fd` as a `File`.
            unsafe { libc::dup2(own.as_raw_fd(), fd) };
            let me = i32::try_from(std::process::id()).expect("a pid");
            // Keep the readiness proof on its own line even after libtest's single-thread prefix.
            println!("\nit reads itself as {:?}", read_process_path(me));
            let _ = std::io::stdin().read_line(&mut line);
        }
        return;
    }
    if let Some(translator) = super::translator()
        && !super::translator_holds_its_program()
    {
        println!(
            "NOT RUN under {translator}: {NAME}: the kernel hands this translator no program \
             it maps executable, so a translated process reads as the translator"
        );
        return;
    }
    let exe = std::env::current_exe().expect("the test binary");
    let mut child = crate::spawnfd::command(&exe)
        .args([
            "--exact",
            NAME,
            "--nocapture",
            "--quiet",
            "--test-threads=1",
        ])
        .env(PARK_ENV, "1")
        .stdin(crate::spawnfd::Stdio::piped())
        .stdout(crate::spawnfd::Stdio::piped())
        .stderr(crate::spawnfd::Stdio::null())
        .spawn()
        .expect("the parked copy");
    let pid = i32::try_from(child.id()).expect("a pid");
    let program = exe.canonicalize().expect("the test binary, canonical");
    let is_program = |read: &Option<PathBuf>| {
        read.as_deref().and_then(|path| path.canonicalize().ok()) == Some(program.clone())
    };
    let until = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let mut read = read_process_path(pid);
    while !is_program(&read) && std::time::Instant::now() < until {
        std::thread::sleep(std::time::Duration::from_millis(10));
        read = read_process_path(pid);
    }
    let mut covered = None;
    if super::translator().is_some() {
        let auxv = std::fs::read(format!("/proc/{pid}/auxv")).expect("the child's auxv");
        let fd = super::procfs::exec_fd(&auxv).expect("the descriptor it was handed");
        let stdin = child.stdin.as_mut().expect("the child's stdin");
        writeln!(stdin, "{fd}").expect("the descriptor, sent");
        crate::deadline::prints(&mut child, ITSELF, crate::deadline::ECHO_HANG);
        let swapped = is_program(&std::fs::read_link(format!("/proc/{pid}/fd/{fd}")).ok())
            && super::read_proc_text(Path::new(&format!("/proc/{pid}/fdinfo/{fd}")))
                .is_ok_and(|info| !super::procfs::exec_opened(&info));
        covered = Some((swapped, read_process_path(pid)));
    }
    drop(child.stdin.take());
    let _ = child.kill();
    let _ = child.wait();
    assert!(
        is_program(&read),
        "the parked copy's program: {read:?}, not {exe:?}"
    );
    if let Some((swapped, read)) = covered {
        assert!(
            swapped,
            "the handed descriptor holds the program the child opened"
        );
        assert_eq!(read, None, "a program the child opened itself is no proof");
    }
}

#[test]
fn linux_reads_the_ignored_signal_mask_as_hex() {
    use super::procfs;
    assert_eq!(
        procfs::ignored_signals("Name:\tsh\nSigIgn:\t0000000000000001\nSigCgt:\t0\n"),
        Some(1)
    );
    assert_eq!(procfs::ignored_signals("SigIgn:\tnot hex\n"), None);
    assert_eq!(procfs::ignored_signals("Name:\tsh\n"), None);
}

#[cfg(target_os = "linux")]
#[test]
fn linux_reads_the_parent_line_and_refuses_a_deleted_exe_link() {
    use super::procfs;
    let status = "Name:\tcat\nUmask:\t0002\nState:\tR (running)\nTgid:\t13\nNgid:\t0\n\
                  Pid:\t13\nPPid:\t4417\nTracerPid:\t0\n";
    assert_eq!(procfs::parent(status), Some(4417));
    assert_eq!(procfs::parent("Name:\tcat\n"), None);
    let live = std::path::PathBuf::from("/opt/aterm/aterm");
    assert_eq!(procfs::live_exe(live.clone()), Some(live));
    assert_eq!(
        procfs::live_exe(std::path::PathBuf::from("/opt/aterm/aterm (deleted)")),
        None
    );
}

#[cfg(target_os = "linux")]
#[test]
fn a_process_on_a_deleted_binary_reads_no_path() {
    let dir = std::env::temp_dir().join(format!(
        "aterm-uds-deleted-exe-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos())
    ));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let copy = dir.join("sleep-copy");
    let copied = crate::spawnfd::command("/bin/cp")
        .arg("/bin/sleep")
        .arg(&copy)
        .status()
        .expect("run cp");
    assert!(copied.success(), "cp /bin/sleep: {copied}");
    let mut child = crate::spawnfd::command(&copy)
        .arg("120")
        .spawn()
        .expect("spawn the copy");
    let pid = i32::try_from(child.id()).expect("pid");
    assert_eq!(read_process_path(pid), Some(copy.clone()), "the control");
    std::fs::remove_file(&copy).expect("delete the binary");
    let path = read_process_path(pid);
    let start = super::read_process_start(pid);
    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(path, None, "a deleted binary names no path");
    assert!(start.is_some(), "the process itself still reads");
}
