// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn under_tier_spawn_cap_is_denied_before_any_process() {
    // SAFETY: single-threaded mint; trusted-launcher contract trivially holds.
    let authority = unsafe { aterm_cap::Authority::root_authority() };
    let weak_spawn = authority.grant::<aterm_cap::effects::Spawn>(aterm_cap::Tier::Untrusted);
    let sandbox_cap = authority.grant::<aterm_sandbox::Sandbox>(aterm_cap::Tier::Trusted);
    let err = spawn_shell(
        24,
        80,
        &weak_spawn,
        &sandbox_cap,
        &[],
        None, // shell_override
        None, // shell_args
        None,
        None,
        None,
        None,
    )
    .expect_err("an under-tier spawn cap must be denied, not spawn a shell");
    assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
}

#[test]
fn under_tier_sandbox_cap_is_denied_fail_closed_no_session() {
    // SAFETY: single-threaded mint; trusted-launcher contract trivially holds.
    let authority = unsafe { aterm_cap::Authority::root_authority() };
    let spawn_cap = authority.grant::<aterm_cap::effects::Spawn>(aterm_cap::Tier::Trusted);
    let weak_sandbox = authority.grant::<aterm_sandbox::Sandbox>(aterm_cap::Tier::Untrusted);
    let sessions_before = registered_here();
    let err = spawn_shell(
        24,
        80,
        &spawn_cap,
        &weak_sandbox,
        &[],
        None, // shell_override
        None, // shell_args
        None,
        None,
        None,
        None,
    )
    .expect_err("a weak sandbox cap must fail closed, not spawn a shell");
    assert_eq!(
        err.kind(),
        io::ErrorKind::PermissionDenied,
        "weak sandbox cap must be PermissionDenied, got: {err}"
    );
    assert!(
        err.to_string().contains("fail-closed"),
        "error must describe the fail-closed refusal: {err}"
    );
    assert_eq!(
        registered_here(),
        sessions_before,
        "no session may be registered for a denied spawn"
    );
}

#[test]
fn sandbox_wrap_demand_fails_closed_unsupported() {
    // SAFETY: single-threaded mint; trusted-launcher contract trivially holds.
    let authority = unsafe { aterm_cap::Authority::root_authority() };
    let spawn_cap = authority.grant::<aterm_cap::effects::Spawn>(aterm_cap::Tier::Trusted);
    let sandbox_cap = authority.grant::<aterm_sandbox::Sandbox>(aterm_cap::Tier::Trusted);
    let err = spawn_shell(
        24,
        80,
        &spawn_cap,
        &sandbox_cap,
        &[],
        None, // shell_override
        None, // shell_args
        None,
        None,
        None,
        Some("(deny network*)"),
    )
    .expect_err("a demanded sandbox wrap must refuse on Windows, not spawn unsandboxed");
    assert_eq!(err.kind(), io::ErrorKind::Unsupported);
    assert!(
        err.to_string().contains("fail-closed"),
        "error must describe the fail-closed refusal: {err}"
    );
}

#[test]
fn a_shell_holding_a_nul_refuses_the_spawn() {
    // SAFETY: single-threaded mint; trusted-launcher contract trivially holds.
    let authority = unsafe { aterm_cap::Authority::root_authority() };
    let spawn_cap = authority.grant::<aterm_cap::effects::Spawn>(aterm_cap::Tier::Trusted);
    let sandbox_cap = authority.grant::<aterm_sandbox::Sandbox>(aterm_cap::Tier::Trusted);
    let args = ["/c".to_string(), "exit".to_string()];
    let over = ["cmd.exe".to_string(), "/c".to_string(), "exit".to_string()];
    for (what, shell_args, argv_override) in [
        ("argv_override", None, Some(&over[..])),
        ("shell_args", Some(&args[..]), None),
        ("bare shell", None, None),
    ] {
        let err = spawn_shell(
            24,
            80,
            &spawn_cap,
            &sandbox_cap,
            &[],
            Some("cmd.exe\0x"),
            shell_args,
            argv_override,
            None,
            None,
            None,
        )
        .expect_err("a shell holding a NUL must refuse, not run another program");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{what}: {err}");
        assert!(
            err.to_string().contains("the shell (set by --shell"),
            "{what}: the refusal names the shell: {err}"
        );
    }
}

const SYNCHRONIZE: u32 = 0x0010_0000;
const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x0000_1000;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

struct TestClient(std::process::Child);

impl TestClient {
    fn spawn() -> Self {
        use std::os::windows::process::CommandExt;
        let exe = std::env::var_os("SystemRoot")
            .map(|r| std::path::Path::new(&r).join("System32").join("sort.exe"))
            .unwrap_or_else(|| std::path::PathBuf::from("sort.exe"));
        #[expect(
            clippy::disallowed_methods,
            reason = "aterm-pty links no aterm-uds on Windows (it calls kernel32 itself); System32's sort.exe as a windowless test client"
        )]
        let child = std::process::Command::new(exe)
            .stdin(std::process::Stdio::piped()) // held open ⇒ it blocks
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .expect("spawning the System32 test client must succeed");
        Self(child)
    }

    fn pid(&self) -> u32 {
        self.0.id()
    }

    fn open(&self) -> isize {
        // SAFETY: OpenProcess against a live child we are holding.
        let h = unsafe {
            ffi::OpenProcess(
                SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION,
                0,
                self.0.id(),
            )
        };
        assert_ne!(h, 0, "OpenProcess on the test client must succeed");
        h
    }

    fn is_alive(&mut self) -> bool {
        matches!(self.0.try_wait(), Ok(None))
    }
}

impl Drop for TestClient {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn one_leave_client_runs_at_a_time() {
    let mut client = TestClient::spawn();
    let mut last = None;
    start_one_leave(&mut last, || Ok(ffi::Handle::new(client.open()))).expect("the first leave");
    let busy = start_one_leave(&mut last, || panic!("a second client while the first runs"))
        .expect_err("held while the first runs");
    assert_eq!(busy.kind(), io::ErrorKind::ResourceBusy);
    let _ = client.0.kill();
    let _ = client.0.wait();
    assert!(!client.is_alive());
    let mut started = false;
    start_one_leave(&mut last, || {
        started = true;
        Ok(ffi::Handle::new(0))
    })
    .expect("the next leave once the first has exited");
    assert!(started);
}

#[test]
fn resize_signal_packet_is_opcode_then_columns_then_rows() {
    let buf = encode_resize_signal(24, 80);
    assert_eq!(buf.len(), 6, "ConPTY's resize signal is exactly 6 bytes");
    assert_eq!(
        &buf[0..2],
        &8u16.to_le_bytes(),
        "opcode must be PTY_SIGNAL_RESIZE_WINDOW (8), little-endian"
    );
    assert_eq!(
        &buf[2..4],
        &80u16.to_le_bytes(),
        "offset 2 is COLUMNS (COORD.X) — not rows"
    );
    assert_eq!(
        &buf[4..6],
        &24u16.to_le_bytes(),
        "offset 4 is ROWS (COORD.Y)"
    );
    assert_eq!(buf, [0x08, 0x00, 0x50, 0x00, 0x18, 0x00]);
}

#[test]
fn resize_signal_is_not_transposed() {
    let buf = encode_resize_signal(1, 999);
    assert_eq!(u16::from_le_bytes([buf[2], buf[3]]), 999, "columns");
    assert_eq!(u16::from_le_bytes([buf[4], buf[5]]), 1, "rows");
}

#[test]
fn resize_signal_clamps_into_positive_coord_range() {
    let zero = encode_resize_signal(0, 0);
    assert_eq!(
        u16::from_le_bytes([zero[2], zero[3]]),
        1,
        "0 cols clamps up to 1"
    );
    assert_eq!(
        u16::from_le_bytes([zero[4], zero[5]]),
        1,
        "0 rows clamps up to 1"
    );

    let huge = encode_resize_signal(u16::MAX, u16::MAX);
    let cols = u16::from_le_bytes([huge[2], huge[3]]);
    let rows = u16::from_le_bytes([huge[4], huge[5]]);
    assert_eq!(cols, 32767, "columns clamp to i16::MAX");
    assert_eq!(rows, 32767, "rows clamp to i16::MAX");
    assert!(
        cols as i16 > 0 && rows as i16 > 0,
        "a clamped COORD must stay positive when reinterpreted as i16"
    );
}

fn keys_read_through_conpty(drive: impl FnOnce(i32)) -> String {
    keys_read_through_conpty_after("", drive)
}

fn keys_read_through_conpty_after(prelude: &str, drive: impl FnOnce(i32)) -> String {
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    // SAFETY: test-process mint; the trusted-launcher contract trivially holds.
    let authority = unsafe { aterm_cap::Authority::root_authority() };
    let spawn_cap = authority.grant::<aterm_cap::effects::Spawn>(aterm_cap::Tier::Trusted);
    let sandbox_cap = authority.grant::<aterm_sandbox::Sandbox>(aterm_cap::Tier::Trusted);
    let script = format!(
        "{prelude}$ui = $Host.UI.RawUI; [Console]::Out.Write('READY'); $keys = @(); \
         do {{ $k = $ui.ReadKey('NoEcho,IncludeKeyDown,IncludeKeyUp'); \
         $keys += ('{{0}}/{{1}}' -f $k.VirtualKeyCode, $k.KeyDown) }} \
         until ($k.VirtualKeyCode -eq 81 -and $k.KeyDown); \
         [Console]::Out.Write('KEYS ' + ($keys -join ';') + ' END')"
    );
    let shell = spawn_shell_with_pid(
        24,
        80,
        &spawn_cap,
        &sandbox_cap,
        &[],
        None,
        None,
        None,
        Some(&[
            "powershell.exe".into(),
            "-NoLogo".into(),
            "-NoProfile".into(),
            "-Command".into(),
            script.into(),
        ]),
        None,
        None,
        aterm_sandbox::Limits::inherit(),
    )
    .expect("ConPTY spawn of powershell.exe must succeed");
    let master = shell.master;
    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        loop {
            let n = read(master, &mut buf);
            if n <= 0 || tx.send(buf[..n as usize].to_vec()).is_err() {
                return;
            }
        }
    });
    fn text_of(stream: &[u8]) -> String {
        let mut text = Vec::new();
        let mut i = 0;
        while i < stream.len() {
            match (stream[i], stream.get(i + 1)) {
                (0x1b, Some(b'[')) => {
                    i += 2;
                    while i < stream.len() && !(0x40..=0x7e).contains(&stream[i]) {
                        i += 1;
                    }
                }
                (0x1b, Some(b']')) => {
                    i += 2;
                    while i < stream.len() && !matches!(stream[i], 0x07 | 0x1b) {
                        i += 1;
                    }
                    if stream.get(i) == Some(&0x1b) {
                        i += 1;
                    }
                }
                (b, _) => text.push(b),
            }
            i += 1;
        }
        String::from_utf8_lossy(&text).into_owned()
    }
    let mut seen = Vec::new();
    let mut wait_for = |marker: &str| {
        let deadline = Instant::now() + Duration::from_secs(60);
        while !text_of(&seen).contains(marker) {
            let left = deadline.saturating_duration_since(Instant::now());
            match rx.recv_timeout(left) {
                Ok(chunk) => seen.extend_from_slice(&chunk),
                Err(_) => {
                    hangup(shell.pid);
                    panic!("no {marker:?} from powershell: {:?}", text_of(&seen));
                }
            }
        }
        text_of(&seen)
    };
    wait_for("READY");
    drive(master);
    let out = wait_for(" END");
    hangup(shell.pid);
    out.split("KEYS ")
        .nth(1)
        .and_then(|tail| tail.split(" END").next())
        .expect("the KEYS report")
        .to_string()
}

#[test]
fn the_first_write_after_a_resize_reaches_the_reader_behind_one_shift_key_up() {
    let keys = keys_read_through_conpty(|master| {
        write_all(master, b"a");
        resize(master, 25, 80);
        write_all(master, b"xq");
    });
    assert_eq!(
        keys, "65/True;65/False;16/False;88/True;88/False;81/True",
        "`a` before the resize arrives as typed; `xq` after it arrives behind \
         one Shift key-up and nothing else"
    );
}

#[test]
fn a_reply_after_a_resize_passes_the_guard_and_the_next_input_carries_it() {
    let keys = keys_read_through_conpty(|master| {
        write_all(master, b"a");
        resize(master, 25, 80);
        as_reply(|| write_all(master, b"x"));
        write_all(master, b"q");
    });
    assert_eq!(
        keys, "65/True;65/False;88/True;88/False;16/False;81/True",
        "the reply `x` arrives as written; the Shift key-up rides the next input"
    );
}

#[test]
fn the_reply_mark_covers_the_closure_and_no_more() {
    let marked = || REPLYING.with(std::cell::Cell::get);
    assert!(!marked());
    assert_eq!(
        as_reply(|| {
            let inner = as_reply(marked);
            (inner, marked())
        }),
        (true, true)
    );
    assert!(!marked());
    let unwound = std::panic::catch_unwind(|| as_reply(|| panic!("a reply writer unwinds")));
    assert!(unwound.is_err());
    assert!(!marked(), "lifted on unwind");
    let other = as_reply(|| std::thread::spawn(marked).join().expect("thread"));
    assert!(!other, "another thread's writes stay input");
}

#[test]
fn a_lone_esc_after_the_guard_record_reaches_the_reader_as_escape() {
    let keys = keys_read_through_conpty(|master| {
        write_all(master, b"a");
        resize(master, 25, 80);
        write_all(master, b"x");
        write_all(master, b"\x1b");
        std::thread::sleep(std::time::Duration::from_millis(500));
        write_all(master, b"q");
    });
    assert_eq!(
        keys, "65/True;65/False;16/False;88/True;88/False;16/False;27/True;27/False;81/True",
        "after the switch a lone ESC is Escape (27), not held into Alt+Q"
    );
}

fn runtime_standin(what: &str) -> (std::path::PathBuf, String) {
    let root = std::env::temp_dir().join(format!("aterm-{what}-{}", std::process::id()));
    let bin = root.join("usr").join("bin");
    std::fs::create_dir_all(&bin).expect("stand-in dir");
    let system = std::env::var_os("SystemRoot").expect("SystemRoot");
    let cmd = std::path::Path::new(&system)
        .join("System32")
        .join("cmd.exe");
    std::fs::copy(&cmd, bin.join("cmd.exe")).expect("copy cmd.exe");
    std::fs::write(bin.join("msys-2.0.dll"), b"").expect("stand-in runtime");
    let prelude = format!(
        "Start-Process -NoNewWindow -FilePath '{}' -ArgumentList '/d /c ping -n 60 127.0.0.1 >nul'; ",
        bin.join("cmd.exe").display()
    );
    (root, prelude)
}

#[test]
fn a_resize_no_input_follows_sends_the_record_by_itself() {
    use std::time::{Duration, Instant};
    let (root, prelude) = runtime_standin("nudge");
    let keys = keys_read_through_conpty_after(&prelude, |master| {
        write_all(master, b"a");
        resize(master, 25, 80);
        let s = session(master).expect("the live session");
        let deadline = Instant::now() + Duration::from_secs(60);
        while s.resize_guard.records_owed() == 2 {
            assert!(Instant::now() < deadline, "no record went by itself");
            std::thread::sleep(Duration::from_millis(10));
        }
        write_all(master, b"q");
    });
    assert_eq!(
        keys, "65/True;65/False;16/False;16/False;81/True",
        "one Shift key-up by itself, then `q` behind the second"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn an_astral_character_reaches_the_reader_as_its_surrogate_records() {
    const RECORDS: &str = "0/True;0/True;0/False;0/False;81/True";
    let (root, prelude) = runtime_standin("astral");
    let typed = |master| {
        write_all(master, "😀".as_bytes());
        write_all(master, b"q");
    };
    let keys = keys_read_through_conpty_after(&prelude, typed);
    assert_eq!(keys, RECORDS, "a runtime in the tab: the records");
    let plain = keys_read_through_conpty(typed);
    assert_ne!(plain, RECORDS, "no runtime in the tab: the text, as typed");
    assert!(plain.ends_with("81/True"), "{plain}");
    let _ = std::fs::remove_dir_all(&root);
}

fn registered_here() -> u64 {
    REGISTERED_HERE.with(std::cell::Cell::get)
}

/// A session dropped before its waiter hung its console up (the waiter thread failed to
/// start) closes its `HPCON` as it drops, once, and the drop returns although nothing reads
/// its output: its own pipe ends close first, so conhost's last frame cannot hold the close.
/// NEGATIVE CONTROL: a drop that closes only the session's handles leaves the pseudoconsole,
/// and its conhost, open; one that closes the pseudoconsole before the output pipe it never
/// read can wait on that frame for good.
#[test]
fn a_dropped_session_closes_its_pseudoconsole() {
    let (pcon, pipes) = create_pseudoconsole(coord(80, 24), OsStr::new("cmd.exe"), false)
        .expect("an inbox pseudoconsole");
    let Pseudoconsole::Inbox { hpc, conhost } = pcon else {
        panic!("a pseudoconsole that may not use OpenConsole is the inbox one");
    };
    let hpc = hpc.release();
    assert_ne!(hpc, 0, "CreatePseudoConsole names its HPCON");
    // As a spawn leaves them: conhost holds the child's ends, the session ours, unread.
    let Pipes {
        in_read,
        in_write,
        out_read,
        out_write,
    } = pipes;
    drop((in_read, out_write));
    let (input, output, conhost) = (in_write.into_raw(), out_read.into_raw(), conhost.into_raw());
    let (dropped, done) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        drop(WinSession {
            hpc: Mutex::new(hpc),
            signal: Mutex::new(0),
            reference: Mutex::new(0),
            host: ConsoleHostKind::Inbox,
            input,
            output,
            process: 0,
            conhost,
            job: 0,
            outside_job_ended: Mutex::new(false),
            close_evt: 0,
            pid: 0,
            exit_code: Mutex::new(None),
            resize_guard: ResizeGuard::new(None, ConsoleHostKind::Inbox),
            input_severed: AtomicBool::new(false),
            leave_client: Mutex::new(None),
            input_writes: std::sync::atomic::AtomicU64::new(0),
        });
        let _ = dropped.send(CLOSED_PSEUDOCONSOLES.with(|closed| closed.borrow().clone()));
    });
    assert_eq!(
        done.recv_timeout(std::time::Duration::from_secs(60)),
        Ok(vec![hpc]),
        "the drop closes the HPCON once and returns with its output unread"
    );
}

#[test]
fn adopt_handoff_without_a_client_process_is_refused_and_registers_nothing() {
    let before = registered_here();
    let err = adopt_handoff(0, 0, 0, 0)
        .expect_err("a handoff with no client process handle must be refused");
    assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    assert_eq!(
        registered_here(),
        before,
        "a refused handoff must not register a session"
    );

    // SAFETY: OpenProcess on our own pid yields a real, closable handle.
    let me = unsafe {
        ffi::OpenProcess(
            SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION,
            0,
            ffi::GetCurrentProcessId(),
        )
    };
    assert_ne!(me, 0, "OpenProcess on self must succeed");
    let (mut rd, mut wr): (isize, isize) = (0, 0);
    // SAFETY: two out-params, default attrs, default buffer size.
    assert_ne!(
        unsafe { ffi::CreatePipe(&mut rd, &mut wr, std::ptr::null_mut(), 0) },
        0,
        "CreatePipe must succeed"
    );
    let sh = adopt_handoff(0, 0, wr, me).expect("a handoff with a client process is adopted");
    assert_eq!(
        registered_here(),
        before + 1,
        "the count sees an accepted handoff's registration"
    );
    close_master(sh.master);
    // SAFETY: the read end is still ours (the session never owned it).
    unsafe { ffi::CloseHandle(rd) };
}

#[test]
fn backend_is_conpty_follows_the_registry() {
    assert!(!backend_is_conpty(-1), "a negative key is nobody's session");
    assert!(
        !backend_is_conpty(0x7fff_ffff),
        "a fabricated key is not a session"
    );

    // SAFETY: OpenProcess on our own pid yields a real, closable handle.
    let me = unsafe {
        ffi::OpenProcess(
            SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION,
            0,
            ffi::GetCurrentProcessId(),
        )
    };
    assert_ne!(me, 0, "OpenProcess on self must succeed");
    let (mut rd, mut wr): (isize, isize) = (0, 0);
    // SAFETY: two out-params, default attrs, default buffer size.
    assert_ne!(
        unsafe { ffi::CreatePipe(&mut rd, &mut wr, std::ptr::null_mut(), 0) },
        0,
        "CreatePipe must succeed"
    );
    let sh = adopt_handoff(0, 0, wr, me).expect("adopt_handoff must accept a real client handle");
    assert!(
        backend_is_conpty(sh.master),
        "a registered (adopted) session is a ConPTY backend"
    );
    close_master(sh.master);
    assert!(
        !backend_is_conpty(sh.master),
        "a closed session is unregistered and no longer answers ConPTY"
    );
    // SAFETY: the read end is still ours (the session never owned it).
    unsafe { ffi::CloseHandle(rd) };
}

#[test]
fn adopted_session_has_no_hpcon_no_job_and_keeps_the_signal_pipe() {
    // SAFETY: OpenProcess on our own pid yields a real, closable handle.
    let me = unsafe {
        ffi::OpenProcess(
            SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION,
            0,
            ffi::GetCurrentProcessId(),
        )
    };
    assert_ne!(me, 0, "OpenProcess on self must succeed");
    let (mut rd, mut wr): (isize, isize) = (0, 0);
    // SAFETY: two out-params, default attrs, default buffer size.
    assert_ne!(
        unsafe { ffi::CreatePipe(&mut rd, &mut wr, std::ptr::null_mut(), 0) },
        0,
        "CreatePipe must succeed"
    );

    let sh = adopt_handoff(0, 0, wr, me).expect("adopt_handoff must accept a real client handle");
    let s = session(sh.master).expect("the adopted session must be registered");
    assert_eq!(
        *s.hpc.lock_or_recover(),
        0,
        "an adopted session must hold NO HPCON — conhost owns the pseudoconsole"
    );
    assert_eq!(
        *s.signal.lock_or_recover(),
        wr,
        "an adopted session must keep the signal pipe it was handed"
    );
    assert_eq!(
        s.job, 0,
        "an adopted session must own no job (never sweep someone else's process)"
    );
    assert_eq!(
        s.conhost, 0,
        "no host handle is plumbed through this signature"
    );
    assert_eq!(
        s.pid,
        unsafe { ffi::GetCurrentProcessId() },
        "the pid must be derived from the client process HANDLE"
    );

    resize(sh.master, 24, 80);
    let mut buf = [0u8; 6];
    let mut got: u32 = 0;
    // SAFETY: `rd` is the live read end; buffer is sized for the 6-byte packet.
    let ok = unsafe { ffi::ReadFile(rd, buf.as_mut_ptr(), 6, &mut got, std::ptr::null_mut()) };
    assert_ne!(
        ok, 0,
        "the resize packet must have been written to the signal pipe"
    );
    assert_eq!(got, 6, "exactly 6 bytes");
    assert_eq!(
        buf,
        [0x08, 0x00, 0x50, 0x00, 0x18, 0x00],
        "resize() must put opcode/cols/rows on the signal pipe verbatim"
    );

    for _ in 0..50 {
        assert_ne!(
            *s.signal.lock_or_recover(),
            0,
            "the waiter tore the adopted session down while its client was still alive"
        );
        std::thread::sleep(std::time::Duration::from_millis(2));
    }

    close_master(sh.master);
    assert!(
        session(sh.master).is_none(),
        "close_master must unregister the adopted session"
    );
    // SAFETY: the read end is still ours (the session never owned it).
    unsafe { ffi::CloseHandle(rd) };
}

#[test]
fn hangup_tears_down_an_adopted_session_through_the_signal_pipe() {
    let mut client = TestClient::spawn();
    let (mut rd, mut wr): (isize, isize) = (0, 0);
    // SAFETY: two out-params, default attrs/size.
    assert_ne!(
        unsafe { ffi::CreatePipe(&mut rd, &mut wr, std::ptr::null_mut(), 0) },
        0
    );
    let sh = adopt_handoff(0, 0, wr, client.open()).expect("adopt");
    let s = session(sh.master).expect("registered");
    assert_eq!(
        sh.pid as u32,
        client.pid(),
        "the adopted session must key on the CLIENT's pid"
    );
    assert_ne!(
        sh.pid as u32,
        // SAFETY: no-argument pid query.
        unsafe { ffi::GetCurrentProcessId() },
        "the fixture must not borrow the test process's pid (see TestClient)"
    );

    hangup(sh.pid);

    let mut closed = false;
    for _ in 0..12_000 {
        if *s.signal.lock_or_recover() == 0 {
            closed = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(
        closed,
        "hangup must reach the waiter and close the adopted session's signal pipe"
    );
    assert!(
        client.is_alive(),
        "the client must still be running, or the teardown proves nothing"
    );
    close_master(sh.master);
    // SAFETY: the read end was never owned by the session.
    unsafe { ffi::CloseHandle(rd) };
}

#[test]
fn drain_available_collects_what_the_pipe_holds_and_never_blocks() {
    let client = TestClient::spawn();
    let pipe = || {
        let (mut rd, mut wr): (isize, isize) = (0, 0);
        // SAFETY: valid out-pointers and default security attributes for `CreatePipe`.
        assert_ne!(
            unsafe { ffi::CreatePipe(&mut rd, &mut wr, std::ptr::null_mut(), 64 * 1024) },
            0,
            "CreatePipe must succeed"
        );
        (rd, wr)
    };
    let (out_rd, out_wr) = pipe();
    let (sig_rd, sig_wr) = pipe();
    let sh = adopt_handoff(0, out_rd, sig_wr, client.open())
        .expect("adopt_handoff must accept a real client handle");
    let write = |bytes: &[u8]| {
        let mut put: u32 = 0;
        // SAFETY: our own live write end; `bytes` outlives the call.
        let ok = unsafe {
            ffi::WriteFile(
                out_wr,
                bytes.as_ptr(),
                u32::try_from(bytes.len()).expect("small"),
                &mut put,
                std::ptr::null_mut(),
            )
        };
        assert!(
            ok != 0 && put as usize == bytes.len(),
            "the write lands whole"
        );
    };
    let master = sh.master;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = [0u8; 64];
        let _ = tx.send(drain_available(master, &mut buf, 5));
    });
    assert_eq!(
        rx.recv_timeout(std::time::Duration::from_secs(60)),
        Ok(5),
        "an empty pipe leaves the fill as it was, without blocking"
    );
    for line in [&b"one\r\n"[..], b"two\r\n", b"six\r\n"] {
        write(line);
    }
    let mut buf = [0u8; 64];
    buf[..2].copy_from_slice(b">>");
    let filled = drain_available(sh.master, &mut buf, 2);
    assert_eq!(&buf[..filled], b">>one\r\ntwo\r\nsix\r\n");
    write(&[b'x'; 100]);
    let mut small = [0u8; 40];
    assert_eq!(drain_available(sh.master, &mut small, 0), 40);
    assert_eq!(drain_available(sh.master, &mut small, 0), 40);
    assert_eq!(drain_available(sh.master, &mut small, 10), 30);
    assert_eq!(drain_available(sh.master, &mut small, 0), 0, "drained");
    close_master(sh.master);
    assert_eq!(drain_available(sh.master, &mut small, 7), 7);
    // SAFETY: the ends the session never owned.
    unsafe {
        ffi::CloseHandle(out_wr);
        ffi::CloseHandle(sig_rd);
    }
}

#[test]
fn a_pid_claimed_by_two_live_sessions_resolves_to_neither() {
    let client = TestClient::spawn();
    let mut pipes = Vec::new();
    let mut sessions = Vec::new();
    for _ in 0..2 {
        let (mut rd, mut wr): (isize, isize) = (0, 0);
        // SAFETY: two out-params, default attrs/size.
        assert_ne!(
            unsafe { ffi::CreatePipe(&mut rd, &mut wr, std::ptr::null_mut(), 0) },
            0
        );
        pipes.push(rd);
        let sh = adopt_handoff(0, 0, wr, client.open()).expect("adopt");
        let s = session(sh.master).expect("registered");
        assert_eq!(sh.pid as u32, client.pid());
        sessions.push((sh, s));
    }
    let pid = sessions[0].0.pid;

    hangup(pid);
    for _ in 0..50 {
        for (i, (_, s)) in sessions.iter().enumerate() {
            assert_ne!(
                *s.signal.lock_or_recover(),
                0,
                "hangup on a pid two live sessions claim tore down session {i}"
            );
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    assert_eq!(
        exit_code(pid),
        None,
        "an ambiguous pid must not report some other session's exit code"
    );
    reap(pid);

    let (sh0, _s0) = sessions.remove(0);
    close_master(sh0.master);
    let (sh1, s1) = &sessions[0];
    hangup(sh1.pid);
    let mut closed = false;
    for _ in 0..12_000 {
        if *s1.signal.lock_or_recover() == 0 {
            closed = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(
        closed,
        "once only one session claims the pid, hangup must reach it again"
    );

    let masters: Vec<i32> = sessions.iter().map(|(sh, _)| sh.master).collect();
    drop(sessions);
    for m in masters {
        close_master(m);
    }
    for rd in pipes {
        // SAFETY: the read ends were never owned by a session.
        unsafe { ffi::CloseHandle(rd) };
    }
}

#[test]
fn release_handoff_handles_closes_the_live_slots_and_skips_the_zeros() {
    const HANG_DETECTOR: std::time::Duration = std::time::Duration::from_secs(60);
    let mut reads = Vec::new();
    let mut writes = Vec::new();
    for _ in 0..2 {
        let (mut rd, mut wr): (isize, isize) = (0, 0);
        // SAFETY: two out-params, default attrs/size.
        assert_ne!(
            unsafe { ffi::CreatePipe(&mut rd, &mut wr, std::ptr::null_mut(), 0) },
            0
        );
        reads.push(rd);
        writes.push(wr);
    }
    release_handoff_handles([0, writes[0], writes[1], 0]);

    for (i, rd) in reads.iter().copied().enumerate() {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut buf = [0u8; 1];
            let mut got: u32 = 0;
            // SAFETY: `rd` is the live read end; 1-byte buffer.
            let ok =
                unsafe { ffi::ReadFile(rd, buf.as_mut_ptr(), 1, &mut got, std::ptr::null_mut()) };
            let _ = tx.send(ok);
        });
        let ok = rx.recv_timeout(HANG_DETECTOR).unwrap_or_else(|_| {
            panic!("slot {i}: ReadFile blocked, so its write end was never closed")
        });
        assert_eq!(ok, 0, "slot {i}: the read end must report broken pipe");
        // SAFETY: the read ends are ours alone.
        unsafe { ffi::CloseHandle(rd) };
    }
}

#[test]
fn adopted_session_tolerates_a_process_handle_with_no_query_rights() {
    // SAFETY: `OpenProcess(SYNCHRONIZE, ...)` on our own PID yields a valid closable handle.
    let me = unsafe { ffi::OpenProcess(SYNCHRONIZE, 0, ffi::GetCurrentProcessId()) };
    assert_ne!(me, 0, "OpenProcess(SYNCHRONIZE) on self must succeed");

    let sh =
        adopt_handoff(0, 0, 0, me).expect("a query-less process handle must still be adoptable");
    let s = session(sh.master).expect("the adopted session must be registered");
    assert_eq!(
        s.pid, 0,
        "GetProcessId cannot see through a SYNCHRONIZE-only handle"
    );
    assert_eq!(
        sh.pid, 0,
        "the unknown pid is reported as 0, not fabricated"
    );

    resize(sh.master, 24, 80);
    close_master(sh.master);
    assert!(
        session(sh.master).is_none(),
        "close_master must unregister it"
    );
}

#[test]
fn fabricated_master_keys_get_defined_miss_semantics() {
    let mut buf = [0u8; 8];
    assert_eq!(read(22, &mut buf), -1, "read: -1 (error half of contract)");
    assert_eq!(
        write_some(22, b"x").expect_err("must err").kind(),
        io::ErrorKind::NotFound
    );
    write_all(22, b"x"); // silent no-op
    resize(22, 24, 80); // no-op
    set_focus_boost(22, true); // no-op
    set_focus_boost(22, false); // no-op
    hangup(22); // no-op (unknown pid)
    reap(22); // no-op
    assert_eq!(exit_code(22), None);
    close_master(22); // no-op
}
