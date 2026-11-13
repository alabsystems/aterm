// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::io::{Read, Write};
use std::os::fd::FromRawFd;

#[test]
fn run_returns_the_shells_status_not_an_older_zombie_childs() {
    const CHILD: &str = "ATERM_TEST_DRIVER_REAPS_THE_SHELL";
    if std::env::var_os(CHILD).is_none() {
        let name = format!(
            "{}::run_returns_the_shells_status_not_an_older_zombie_childs",
            module_path!().split_once("::").map_or("", |(_, rest)| rest)
        );
        let mut command =
            aterm_uds::spawnfd::command(std::env::current_exe().expect("test binary"));
        command
            .args(["--exact", &name, "--nocapture", "--test-threads=1"])
            .env(CHILD, "1")
            .stdin(aterm_uds::spawnfd::Stdio::null());
        let out =
            aterm_uds::deadline::output_within(&mut command, std::time::Duration::from_secs(60))
                .expect("re-exec the test binary");
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(out.status.success(), "the re-exec failed:\n{text}");
        assert!(
            text.contains("1 passed"),
            "the re-exec ran no test:\n{text}"
        );
        return;
    }
    let decoy = aterm_uds::spawnfd::command("/bin/sh")
        .args(["-c", "exit 7"])
        .spawn()
        .expect("decoy");
    let decoy_pid = libc::pid_t::try_from(decoy.id()).expect("pid");
    std::mem::forget(decoy);
    let decoy_id = libc::id_t::try_from(decoy_pid).expect("id");
    loop {
        // SAFETY: `siginfo_t` is plain old data and valid when zeroed.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        // SAFETY: `waitid` on `decoy` writes into valid local `info`.
        let r = unsafe {
            libc::waitid(
                libc::P_PID,
                decoy_id,
                &mut info,
                libc::WEXITED | libc::WNOWAIT,
            )
        };
        if r == 0 {
            break;
        }
        let e = std::io::Error::last_os_error();
        assert_eq!(
            e.kind(),
            std::io::ErrorKind::Interrupted,
            "waitid on the decoy: {e}"
        );
    }

    // SAFETY: isolated test child launch point.
    let authority = unsafe { aterm_cap::Authority::root_authority() };
    let spawn_cap = authority.grant::<aterm_cap::effects::Spawn>(aterm_cap::Tier::Trusted);
    let sandbox_cap = authority.grant::<aterm_sandbox::Sandbox>(aterm_cap::Tier::Trusted);
    let command: [std::ffi::OsString; 3] = [
        "/bin/sh".into(),
        "-c".into(),
        "trap '' HUP; exec 1>&- 2>&-; read -r release; exit 3".into(),
    ];
    let spawn = |command: &[std::ffi::OsString]| {
        aterm_pty::spawn_shell_with_pid(
            24,
            80,
            &spawn_cap,
            &sandbox_cap,
            &[],
            None,
            None,
            None,
            Some(command),
            None,
            None,
            aterm_sandbox::Limits::shell_default(),
        )
        .expect("protected shell spawn")
    };
    let shell = spawn(&command);
    // No shell output or exit can wake this session until its production maintenance
    // deadline releases the quiet read. The signal pipe otherwise permits an infinite poll.
    assert!(wake_pipe().is_some());
    let now = std::time::Instant::now();
    assert_eq!(maintenance_wait(-1, None, now), -1);
    assert_eq!(maintenance_wait(-1, Some(now), now), 0);
    assert_eq!(
        maintenance_wait(-1, Some(now + std::time::Duration::from_nanos(1)), now),
        1
    );
    assert_eq!(
        maintenance_wait(100, Some(now + std::time::Duration::from_secs(1)), now),
        100
    );
    let master = shell.master;
    let released = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    struct Released(std::sync::Arc<std::sync::atomic::AtomicBool>);
    impl Drop for Released {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }
    let owned = Released(std::sync::Arc::clone(&released));
    let attempts = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let tried = std::sync::Arc::clone(&attempts);
    let mut due = None;
    let mut done = false;
    let maintenance = Box::new(move || {
        let _keep = &owned;
        if done {
            return None;
        }
        let now = std::time::Instant::now();
        let Some(deadline) = due else {
            tried.fetch_add(1, Ordering::SeqCst);
            let deadline = now + std::time::Duration::from_millis(20);
            due = Some(deadline);
            return due;
        };
        if now < deadline {
            return due;
        }
        tried.fetch_add(1, Ordering::SeqCst);
        write_all(master, b"release\n");
        done = true;
        None
    });
    let code = run(shell, false, Some(maintenance));
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    assert!(
        released.load(Ordering::SeqCst),
        "driver owns and releases maintenance before process exit"
    );

    // A panicking host callback still restores the terminal and settles only its shell.
    let (host, slave) = aterm_pty::open_pty_pair().expect("private host terminal");
    // SAFETY: keep this isolated test child's stdin and install its owned terminal.
    let stdin = unsafe { libc::fcntl(libc::STDIN_FILENO, libc::F_DUPFD_CLOEXEC, 3) };
    assert!(stdin >= 0);
    // SAFETY: `slave` is this fixture's live terminal descriptor.
    assert_eq!(
        unsafe { libc::dup2(slave, libc::STDIN_FILENO) },
        libc::STDIN_FILENO
    );
    aterm_pty::close_fd(slave);
    let attributes = || {
        // SAFETY: `termios` is initialized by the successful read from our private terminal.
        let mut t: libc::termios = unsafe { std::mem::zeroed() };
        assert_eq!(unsafe { libc::tcgetattr(libc::STDIN_FILENO, &mut t) }, 0);
        // Darwin's sys/termios.h classifies PENDIN as pending-input state; restoring
        // canonical mode may set it. Compare every setting, without this kernel state bit.
        #[cfg(target_os = "macos")]
        {
            t.c_lflag &= !libc::PENDIN;
        }
        (t.c_iflag, t.c_oflag, t.c_cflag, t.c_lflag, t.c_cc)
    };
    let original = attributes();
    assert_ne!(
        original.3 & libc::ICANON,
        0,
        "the private host starts canonical"
    );
    let shell = spawn(&[
        "/bin/sh".into(),
        "-c".into(),
        "printf READY; read -r release; exit 4".into(),
    ]);
    let mut ready = Vec::new();
    while ready.len() < 5 {
        let mut chunk = [0; 5];
        let n = aterm_pty::read(shell.master, &mut chunk);
        assert!(n > 0, "the owned shell ended before its quiet read");
        ready.extend_from_slice(&chunk[..n as usize]);
    }
    assert_eq!(ready, b"READY", "the owned shell reached its quiet read");
    let (master, pid) = (shell.master, shell.pid);
    let released = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let owned = Released(std::sync::Arc::clone(&released));
    let raw = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let raw_seen = std::sync::Arc::clone(&raw);
    let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run(
            shell,
            false,
            Some(Box::new(move || {
                let _keep = &owned;
                raw_seen.store(attributes().3 & libc::ICANON == 0, Ordering::SeqCst);
                panic!("the owned maintenance unwound");
            })),
        )
    }));
    assert!(failed.is_err());
    assert!(raw.load(Ordering::SeqCst), "the callback ran in raw mode");
    assert_eq!(attributes(), original, "raw mode was restored on unwind");
    assert!(released.load(Ordering::SeqCst));
    // SAFETY: query only the descriptor and exact child owned by this driver invocation.
    assert_eq!(unsafe { libc::fcntl(master, libc::F_GETFD) }, -1);
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::EBADF)
    );
    let mut status = 0;
    // SAFETY: the queried pid is the protected shell this invocation owned.
    assert_eq!(
        unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) },
        -1
    );
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ECHILD)
    );
    // SAFETY: restore the isolated fixture's saved stdin before closing its private terminal.
    assert_eq!(
        unsafe { libc::dup2(stdin, libc::STDIN_FILENO) },
        libc::STDIN_FILENO
    );
    aterm_pty::close_fd(stdin);
    aterm_pty::close_fd(host);
    let mut status = 0;
    unsafe { libc::waitpid(decoy_pid, &mut status, libc::WNOHANG) };
    assert_eq!(code, 3, "the session took another child's exit status");
}

#[test]
fn the_shells_first_output_runs_the_hook_once() {
    use std::sync::atomic::AtomicUsize;
    const CHILD: &str = "ATERM_TEST_DRIVER_FIRST_OUTPUT_HOOK";
    static RAN: AtomicUsize = AtomicUsize::new(0);
    if std::env::var_os(CHILD).is_none() {
        let name = format!(
            "{}::the_shells_first_output_runs_the_hook_once",
            module_path!().split_once("::").map_or("", |(_, rest)| rest)
        );
        let out = aterm_uds::spawnfd::command(std::env::current_exe().expect("test binary"))
            .args(["--exact", &name, "--nocapture", "--test-threads=1"])
            .env(CHILD, "1")
            .stdin(aterm_uds::spawnfd::Stdio::null())
            .output()
            .expect("re-exec the test binary");
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(out.status.success(), "the re-exec failed:\n{text}");
        assert!(
            text.contains("1 passed"),
            "the re-exec ran no test:\n{text}"
        );
        return;
    }
    assert!(crate::on_first_shell_output(|| {
        RAN.fetch_add(1, Ordering::SeqCst);
    }));
    assert!(
        !crate::on_first_shell_output(|| {}),
        "the first registration wins"
    );
    // SAFETY: isolated test child launch point.
    let authority = unsafe { aterm_cap::Authority::root_authority() };
    let spawn_cap = authority.grant::<aterm_cap::effects::Spawn>(aterm_cap::Tier::Trusted);
    let sandbox_cap = authority.grant::<aterm_sandbox::Sandbox>(aterm_cap::Tier::Trusted);
    let command: [std::ffi::OsString; 3] = [
        "/bin/sh".into(),
        "-c".into(),
        "printf one; sleep 0.2; printf two; sleep 0.2; printf three".into(),
    ];
    let shell = aterm_pty::spawn_shell_with_pid(
        24,
        80,
        &spawn_cap,
        &sandbox_cap,
        &[],
        None,
        None,
        None,
        Some(&command),
        None,
        None,
        aterm_sandbox::Limits::shell_default(),
    )
    .expect("protected shell spawn");
    assert_eq!(
        RAN.load(Ordering::SeqCst),
        0,
        "nothing before the shell speaks"
    );
    assert_eq!(run(shell, false, None), 0);
    assert_eq!(RAN.load(Ordering::SeqCst), 1, "once, however much it said");
}

#[test]
fn input_the_program_never_reads_does_not_stop_its_output() {
    const CHILD: &str = "ATERM_TEST_DRIVER_UNREAD_INPUT";
    const LINES: usize = 4000;
    if std::env::var_os(CHILD).is_none() {
        let name = format!(
            "{}::input_the_program_never_reads_does_not_stop_its_output",
            module_path!().split_once("::").map_or("", |(_, rest)| rest)
        );
        let mut child = aterm_uds::spawnfd::command(std::env::current_exe().expect("test binary"))
            .args(["--exact", &name, "--nocapture", "--test-threads=1"])
            .env(CHILD, "1")
            .stdin(aterm_uds::spawnfd::Stdio::piped())
            .stdout(aterm_uds::spawnfd::Stdio::piped())
            .stderr(aterm_uds::spawnfd::Stdio::piped())
            .spawn()
            .expect("re-exec the test binary");
        let mut stdin = child.stdin.take().expect("stdin");
        let feeder = std::thread::spawn(move || {
            let line = [b"typed-ahead input nobody reads".as_slice(), b"\n"].concat();
            for _ in 0..(256 * 1024 / line.len()) {
                if stdin.write_all(&line).is_err() {
                    return;
                }
            }
        });
        let drain = |mut from: Box<dyn Read + Send>| {
            std::thread::spawn(move || {
                let mut all = Vec::new();
                let _ = from.read_to_end(&mut all);
                all
            })
        };
        let stdout = drain(Box::new(child.stdout.take().expect("stdout")));
        let stderr = drain(Box::new(child.stderr.take().expect("stderr")));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        let status = loop {
            if let Some(status) = child.try_wait().expect("try_wait") {
                break Some(status);
            }
            if std::time::Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        };
        let _ = feeder.join();
        let out = stdout.join().expect("stdout drain");
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out),
            String::from_utf8_lossy(&stderr.join().expect("stderr drain"))
        );
        let status = status.unwrap_or_else(|| {
            panic!(
                "the session hung: the program's output stopped behind input it never \
                 reads ({} bytes passed through)",
                out.len()
            )
        });
        assert!(status.success(), "the re-exec failed:\n{text}");
        assert!(
            text.contains("1 passed"),
            "the re-exec ran no test:\n{text}"
        );
        let last = format!("out{}", LINES - 1);
        assert!(
            String::from_utf8_lossy(&out).contains(&last),
            "the program's last line never passed through"
        );
        return;
    }
    // SAFETY: isolated test child launch point.
    let authority = unsafe { aterm_cap::Authority::root_authority() };
    let spawn_cap = authority.grant::<aterm_cap::effects::Spawn>(aterm_cap::Tier::Trusted);
    let sandbox_cap = authority.grant::<aterm_sandbox::Sandbox>(aterm_cap::Tier::Trusted);
    let command: [std::ffi::OsString; 3] = [
        "/bin/sh".into(),
        "-c".into(),
        format!("i=0; while [ $i -lt {LINES} ]; do echo out$i; i=$((i+1)); done; exit 5").into(),
    ];
    let shell = aterm_pty::spawn_shell_with_pid(
        24,
        80,
        &spawn_cap,
        &sandbox_cap,
        &[],
        None,
        None,
        None,
        Some(&command),
        None,
        None,
        aterm_sandbox::Limits::shell_default(),
    )
    .expect("protected shell spawn");
    assert_eq!(
        run(shell, false, None),
        5,
        "the session returns the program's status"
    );
}

#[test]
fn a_resize_before_the_driver_listens_reaches_the_program() {
    const CHILD: &str = "ATERM_TEST_DRIVER_EARLY_RESIZE";
    if std::env::var_os(CHILD).is_none() {
        let name = format!(
            "{}::a_resize_before_the_driver_listens_reaches_the_program",
            module_path!().split_once("::").map_or("", |(_, rest)| rest)
        );
        let out = aterm_uds::spawnfd::command(std::env::current_exe().expect("test binary"))
            .args(["--exact", &name, "--nocapture", "--test-threads=1"])
            .env(CHILD, "1")
            .stdin(aterm_uds::spawnfd::Stdio::null())
            .output()
            .expect("re-exec the test binary");
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(out.status.success(), "the re-exec failed:\n{text}");
        assert!(
            text.contains("1 passed"),
            "the re-exec ran no test:\n{text}"
        );
        return;
    }
    let (host, host_slave) = aterm_pty::open_pty_pair().expect("host pty");
    aterm_pty::resize(host, 30, 100);
    // SAFETY: duplicating descriptors this test owns: the harness's stdout is kept aside for
    // its result line, and the host slave takes its place.
    let harness_stdout = unsafe { libc::fcntl(libc::STDOUT_FILENO, libc::F_DUPFD_CLOEXEC, 3) };
    assert!(harness_stdout >= 0, "keep the harness's stdout");
    // SAFETY: as above.
    assert_eq!(
        unsafe { libc::dup2(host_slave, libc::STDOUT_FILENO) },
        libc::STDOUT_FILENO
    );
    aterm_pty::close_fd(host_slave);
    let drained = std::thread::spawn(move || {
        let mut all = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            let n = aterm_pty::read(host, &mut buf);
            if n <= 0 {
                return all;
            }
            all.extend_from_slice(&buf[..n as usize]);
            if String::from_utf8_lossy(&all).contains("SIZE ") {
                return all;
            }
        }
    });

    // SAFETY: isolated test child launch point.
    let authority = unsafe { aterm_cap::Authority::root_authority() };
    let spawn_cap = authority.grant::<aterm_cap::effects::Spawn>(aterm_cap::Tier::Trusted);
    let sandbox_cap = authority.grant::<aterm_sandbox::Sandbox>(aterm_cap::Tier::Trusted);
    let command: [std::ffi::OsString; 3] = [
        "/bin/sh".into(),
        "-c".into(),
        "trap 'echo SIZE $(stty size); exit 0' WINCH; echo READY; i=0; \
         while [ $i -lt 600 ]; do sleep 0.1; i=$((i+1)); done; \
         echo SIZE $(stty size) unchanged; exit 3"
            .into(),
    ];
    let (rows, cols) = host_winsize();
    assert_eq!((rows, cols), (30, 100), "the spawn reads the host's size");
    let shell = aterm_pty::spawn_shell_with_pid(
        rows,
        cols,
        &spawn_cap,
        &sandbox_cap,
        &[],
        None,
        None,
        None,
        Some(&command),
        None,
        None,
        aterm_sandbox::Limits::shell_default(),
    )
    .expect("protected shell spawn");
    let mut seen = Vec::new();
    let mut buf = [0u8; 256];
    while !String::from_utf8_lossy(&seen).contains("READY") {
        let n = aterm_pty::read(shell.master, &mut buf);
        assert!(n > 0, "the program ended before it was up");
        seen.extend_from_slice(&buf[..n as usize]);
    }
    aterm_pty::resize(host, 40, 120);
    let code = run(shell, false, None);
    // SAFETY: restores `harness_stdout`, a descriptor this test owns, onto `STDOUT_FILENO`.
    unsafe { libc::dup2(harness_stdout, libc::STDOUT_FILENO) };
    aterm_pty::close_fd(harness_stdout);
    let out = drained.join().expect("host drain");
    aterm_pty::close_fd(host);
    let text = String::from_utf8_lossy(&out);
    assert!(
        text.contains("SIZE 40 120"),
        "the program never learned the host's new size: {text:?}"
    );
    assert_eq!(code, 0, "the program saw the resize: {text:?}");
}

fn reexec(name: &str, child: &str, stdin: aterm_uds::spawnfd::Stdio) {
    let name = format!(
        "{}::{name}",
        module_path!().split_once("::").map_or("", |(_, rest)| rest)
    );
    let out = aterm_uds::spawnfd::command(std::env::current_exe().expect("test binary"))
        .args(["--exact", &name, "--nocapture", "--test-threads=1"])
        .env(child, "1")
        .stdin(stdin)
        .output()
        .expect("re-exec the test binary");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "the re-exec failed:\n{text}");
    assert!(
        text.contains("1 passed"),
        "the re-exec ran no test:\n{text}"
    );
}

fn wait_until(what: &str, done: impl Fn() -> bool) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while !done() {
        assert!(
            std::time::Instant::now() < deadline,
            "a minute passed and {what}"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

fn has_exited(pid: libc::pid_t) -> bool {
    // SAFETY: `siginfo_t` is plain old data and valid when zeroed.
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    // SAFETY: non-blocking `WNOWAIT` `waitid` into valid local `info`.
    let r = unsafe {
        libc::waitid(
            libc::P_PID,
            libc::id_t::try_from(pid).expect("pid"),
            &mut info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    // SAFETY: `info` was zero-initialized and populated by `waitid`.
    r == 0 && unsafe { info.si_pid() } == pid
}

/// The slave-side node of the pty whose master is `master`, asked of the kernel by
/// ioctl (`TIOCPTYGNAME` / `ptsname_r`) — never `ttyname`, which scans `/dev` and
/// hangs on one wedged node. A child that must reopen its terminal by name is handed
/// this instead of resolving it itself (the slave-holding job below).
fn slave_name(master: libc::c_int) -> String {
    let mut buf = [0 as libc::c_char; 128];
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    // SAFETY: `TIOCPTYGNAME` writes a NUL-terminated name of at most 128 bytes into
    // `buf`, which has exactly that many.
    let rc = unsafe {
        libc::ioctl(
            master,
            libc::TIOCPTYGNAME as libc::c_ulong,
            buf.as_mut_ptr(),
        )
    };
    #[cfg(not(any(target_os = "macos", target_os = "ios")))]
    // SAFETY: `ptsname_r` writes a NUL-terminated name of at most `buf.len()` bytes.
    let rc = unsafe { libc::ptsname_r(master, buf.as_mut_ptr(), buf.len()) };
    assert_eq!(
        rc,
        0,
        "the slave's name: {}",
        std::io::Error::last_os_error()
    );
    // SAFETY: `rc == 0`: `buf` holds a NUL-terminated name.
    unsafe { std::ffi::CStr::from_ptr(buf.as_ptr()) }
        .to_string_lossy()
        .into_owned()
}

#[test]
fn a_resize_between_the_signal_check_and_the_poll_reaches_the_program() {
    const CHILD: &str = "ATERM_TEST_DRIVER_RESIZE_BEFORE_POLL";
    static HOST: AtomicI32 = AtomicI32::new(-1);
    static FIRED: AtomicBool = AtomicBool::new(false);
    if std::env::var_os(CHILD).is_none() {
        let (stdin, held) = std::io::pipe().expect("stdin pipe");
        reexec(
            "a_resize_between_the_signal_check_and_the_poll_reaches_the_program",
            CHILD,
            stdin.into(),
        );
        drop(held);
        return;
    }
    let (host, host_slave) = aterm_pty::open_pty_pair().expect("host pty");
    aterm_pty::resize(host, 30, 100);
    // SAFETY: duplicates `STDOUT_FILENO` with `F_DUPFD_CLOEXEC`.
    let harness_stdout = unsafe { libc::fcntl(libc::STDOUT_FILENO, libc::F_DUPFD_CLOEXEC, 3) };
    assert!(harness_stdout >= 0, "keep the harness's stdout");
    // SAFETY: duplicates `host_slave` onto `STDOUT_FILENO`.
    assert_eq!(
        unsafe { libc::dup2(host_slave, libc::STDOUT_FILENO) },
        libc::STDOUT_FILENO
    );
    aterm_pty::close_fd(host_slave);
    let drained = std::thread::spawn(move || {
        let mut all = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            let n = aterm_pty::read(host, &mut buf);
            if n <= 0 {
                return all;
            }
            all.extend_from_slice(&buf[..n as usize]);
            let text = String::from_utf8_lossy(&all);
            if text
                .find("SIZE ")
                .is_some_and(|at| text[at..].contains('\n'))
            {
                return all;
            }
        }
    });

    // SAFETY: isolated test child launch point.
    let authority = unsafe { aterm_cap::Authority::root_authority() };
    let spawn_cap = authority.grant::<aterm_cap::effects::Spawn>(aterm_cap::Tier::Trusted);
    let sandbox_cap = authority.grant::<aterm_sandbox::Sandbox>(aterm_cap::Tier::Trusted);
    let command: [std::ffi::OsString; 3] = [
        "/bin/sh".into(),
        "-c".into(),
        "trap 'echo SIZE $(stty size); exit 0' WINCH; printf READY; i=0; \
         while [ $i -lt 600 ]; do sleep 0.1; i=$((i+1)); done; \
         trap '' WINCH; echo SIZE $(stty size) unchanged; exit 3"
            .into(),
    ];
    let shell = aterm_pty::spawn_shell_with_pid(
        30,
        100,
        &spawn_cap,
        &sandbox_cap,
        &[],
        None,
        None,
        None,
        Some(&command),
        None,
        None,
        aterm_sandbox::Limits::shell_default(),
    )
    .expect("protected shell spawn");
    let mut seen = Vec::new();
    let mut buf = [0u8; 256];
    while !String::from_utf8_lossy(&seen).contains("READY") {
        let n = aterm_pty::read(shell.master, &mut buf);
        assert!(n > 0, "the program ended before it was up");
        seen.extend_from_slice(&buf[..n as usize]);
    }
    HOST.store(host, Ordering::SeqCst);
    assert!(
        BEFORE_POLL
            .set(|| {
                if !FIRED.swap(true, Ordering::SeqCst) {
                    aterm_pty::resize(HOST.load(Ordering::SeqCst), 40, 120);
                    // SAFETY: raises `SIGWINCH` on the current thread with async-signal-safe
                    // handler.
                    #[expect(
                        clippy::disallowed_methods,
                        reason = "test code: SIGWINCH, which dumps no core"
                    )]
                    unsafe {
                        libc::raise(libc::SIGWINCH)
                    };
                }
            })
            .is_ok(),
        "the seam is this process's"
    );
    let code = run(shell, false, None);
    // SAFETY: restores `harness_stdout` onto `STDOUT_FILENO`.
    unsafe { libc::dup2(harness_stdout, libc::STDOUT_FILENO) };
    aterm_pty::close_fd(harness_stdout);
    let out = drained.join().expect("host drain");
    aterm_pty::close_fd(host);
    let text = String::from_utf8_lossy(&out);
    assert!(FIRED.load(Ordering::SeqCst), "the seam never ran");
    assert!(
        text.contains("SIZE 40 120"),
        "the resize raised before the poll waited for other I/O: {text:?}"
    );
    assert_eq!(code, 0, "the program saw the resize: {text:?}");
}

#[test]
fn the_session_ends_with_the_shell_while_a_job_holds_its_terminal() {
    const CHILD: &str = "ATERM_TEST_DRIVER_JOB_HOLDS_THE_SLAVE";
    static SHELL: AtomicI32 = AtomicI32::new(-1);
    static DIR: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    static FIRED: AtomicBool = AtomicBool::new(false);
    if std::env::var_os(CHILD).is_none() {
        reexec(
            "the_session_ends_with_the_shell_while_a_job_holds_its_terminal",
            CHILD,
            aterm_uds::spawnfd::Stdio::null(),
        );
        return;
    }
    let hold = aterm_tempfile::Builder::new()
        .prefix("aterm-driver-job-holds-slave-")
        .tempdir()
        .expect("scratch dir");
    let dir = hold.path().to_path_buf();
    // SAFETY: isolated test child launch point.
    let authority = unsafe { aterm_cap::Authority::root_authority() };
    let spawn_cap = authority.grant::<aterm_cap::effects::Spawn>(aterm_cap::Tier::Trusted);
    let sandbox_cap = authority.grant::<aterm_sandbox::Sandbox>(aterm_cap::Tier::Trusted);
    let command: [std::ffi::OsString; 3] = [
        "/bin/sh".into(),
        "-c".into(),
        r#"d=$ATERM_TEST_DIR
            ( trap '' HUP; : >"$d/armed"; exec </dev/null >/dev/null 2>&1
              i=0; while [ ! -e "$d/go" ] && [ $i -lt 600 ]; do sleep 0.1; i=$((i+1)); done
              exec 3<>"$(cat "$d/slave")"; : >"$d/held"
              i=0; while [ ! -e "$d/release" ] && [ $i -lt 600 ]; do sleep 0.1; i=$((i+1)); done
              [ -e "$d/release" ] || : >"$d/held-to-the-end"
              : >"$d/done" ) &
            i=0; while [ ! -e "$d/exit" ] && [ $i -lt 600 ]; do sleep 0.1; i=$((i+1)); done
            exit 7"#
            .into(),
    ];
    let shell = aterm_pty::spawn_shell_with_pid(
        24,
        80,
        &spawn_cap,
        &sandbox_cap,
        &[("ATERM_TEST_DIR".into(), dir.to_string_lossy().into_owned())],
        None,
        None,
        None,
        Some(&command),
        None,
        None,
        aterm_sandbox::Limits::shell_default(),
    )
    .expect("protected shell spawn");
    // The job reopens its terminal by name once `go` lands; the name is the parent's
    // to give (an ioctl on the master, never the child's `ttyname` scan of `/dev`),
    // and written here it is whole before the seam can write `go` — the seam runs
    // only inside `run`'s poll.
    std::fs::write(dir.join("slave"), slave_name(shell.master))
        .expect("name the slave for the job");
    SHELL.store(shell.pid, Ordering::SeqCst);
    assert!(
        DIR.set(dir.clone()).is_ok(),
        "the scratch dir is this process's"
    );
    assert!(
        BEFORE_POLL
            .set(|| {
                if FIRED.swap(true, Ordering::SeqCst) {
                    return;
                }
                let dir = DIR.get().expect("scratch dir");
                wait_until("the job does not ignore SIGHUP yet", || {
                    dir.join("armed").exists()
                });
                std::fs::write(dir.join("exit"), "").expect("let the shell exit");
                wait_until("the shell has not exited", || {
                    has_exited(SHELL.load(Ordering::SeqCst))
                });
                std::fs::write(dir.join("go"), "").expect("let the job open the terminal");
                wait_until("the job has not opened the terminal again", || {
                    dir.join("held").exists()
                });
            })
            .is_ok(),
        "the seam is this process's"
    );
    let code = run(shell, false, None);
    let held_to_the_end = dir.join("held-to-the-end").exists();
    std::fs::write(dir.join("release"), "").expect("let the job go");
    wait_until("the job has not finished", || dir.join("done").exists());
    drop(hold);
    assert!(FIRED.load(Ordering::SeqCst), "the seam never ran");
    assert!(
        !held_to_the_end,
        "the session outlived its shell until the job let go of the terminal"
    );
    assert_eq!(code, 7, "the session returns the shell's status");
}

#[test]
fn every_terminal_poll_flag_reaches_the_read_path() {
    for flag in [libc::POLLHUP, libc::POLLERR, libc::POLLNVAL, libc::POLLIN] {
        assert!(poll_must_read(flag), "flag {flag:#x} must be consumed");
        assert!(
            poll_must_read(flag | libc::POLLOUT),
            "unrelated readiness must not mask {flag:#x}"
        );
    }
    assert!(!poll_must_read(0));
    assert!(!poll_must_read(libc::POLLOUT));
}

#[test]
fn closed_pipe_drains_buffered_bytes_then_consumes_hangup() {
    let mut raw = [-1; 2];
    assert_eq!(unsafe { libc::pipe(raw.as_mut_ptr()) }, 0);
    for fd in raw {
        // SAFETY: `fd` is a valid pipe descriptor from `libc::pipe`.
        assert_eq!(
            unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) },
            0
        );
    }
    // SAFETY: transfers ownership of fresh pipe read fd `raw[0]` to `File`.
    let mut reader = unsafe { std::fs::File::from_raw_fd(raw[0]) };
    // SAFETY: transfers ownership of fresh pipe write fd `raw[1]` to `File`.
    let mut writer = unsafe { std::fs::File::from_raw_fd(raw[1]) };
    let expected = b"buffered-after-close";
    writer.write_all(expected).expect("seed pipe");
    drop(writer);

    let mut got = Vec::new();
    let mut saw_terminal_readiness = false;
    let mut saw_eof = false;
    for _ in 0..expected.len() + 2 {
        let mut event = libc::pollfd {
            fd: raw[0],
            events: libc::POLLIN,
            revents: 0,
        };
        assert_eq!(unsafe { libc::poll(&mut event, 1, 60_000) }, 1);
        assert!(
            poll_must_read(event.revents),
            "revents={:#x}",
            event.revents
        );
        saw_terminal_readiness |=
            event.revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0;
        let mut chunk = [0_u8; 3];
        let n = reader.read(&mut chunk).expect("consume ready pipe");
        if n == 0 {
            saw_eof = true;
            break;
        }
        got.extend_from_slice(&chunk[..n]);
    }

    assert_eq!(got, expected);
    assert!(
        saw_terminal_readiness,
        "the closed writer must surface terminal readiness"
    );
    assert!(saw_eof, "terminal readiness must be consumed through EOF");
}
