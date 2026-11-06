// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for [`Command`]: stdio modes, argument/env/cwd rules, inheritance, signals, and `exec`.

#[cfg(target_vendor = "apple")]
use super::tests::flags;
use super::tests::{
    STAGE_ENV, STD_SPAWN_MUTE, park, require_host_holds, require_soft_limit, run_staged,
    translator, unflagged_at_or_above,
};
use super::*;
use std::io::{Read, Write};
#[cfg(target_vendor = "apple")]
use std::os::fd::AsFd as _;
use std::os::fd::{AsRawFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Darwin may report a start-suspended child's CLD_STOPPED even with WEXITED.
/// Neither that notification nor its pid means the actual owned child exited.
#[cfg(target_vendor = "apple")]
#[test]
fn a_start_suspended_child_is_not_an_exited_child() {
    let mut child = command("/bin/cat")
        .start_suspended(true)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let stopped = child.exited();
    child.resume().unwrap();
    let input = crate::deadline::cat_echoes(&mut child);
    drop(input);
    let status = child.wait().unwrap();
    assert!(!stopped.unwrap(), "CLD_STOPPED is not terminal evidence");
    assert!(status.success());
}

/// The accounting handle owns the wait, including a nonblocking look while a
/// pipe handshake keeps the child alive, and caches status and usage together.
#[cfg(all(
    target_pointer_width = "64",
    any(target_vendor = "apple", target_os = "linux")
))]
#[test]
fn resource_accounting_reaps_once_and_keeps_the_childs_status() {
    const NAME: &str =
        "spawnfd::command_tests::resource_accounting_reaps_once_and_keeps_the_childs_status";
    const ALLOCATED: usize = 8 * 1024 * 1024;
    if std::env::var(STAGE_ENV).as_deref() == Ok("rusage") {
        let mut memory = vec![0u8; ALLOCATED];
        for (n, byte) in memory.iter_mut().enumerate() {
            *byte = n as u8;
        }
        std::hint::black_box(&memory);
        std::io::stdout().write_all(b"ready\n").unwrap();
        std::io::stdout().flush().unwrap();
        let mut go = [0];
        std::io::stdin().read_exact(&mut go).unwrap();
        std::process::exit(7);
    }
    let mut child = command(std::env::current_exe().unwrap())
        .args([NAME, "--exact", "--nocapture"])
        .env(STAGE_ENV, "rusage")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut ready = std::io::BufReader::new(child.stdout.take().unwrap());
    let (sent, received) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        use std::io::BufRead as _;
        let mut line = String::new();
        while ready.read_line(&mut line).is_ok_and(|n| n > 0) {
            if line.ends_with("ready\n") {
                let _ = sent.send(());
                return;
            }
            line.clear();
        }
    });
    received
        .recv_timeout(Duration::from_secs(60))
        .expect("child never reached its handshake");
    assert_eq!(child.try_wait_with_rusage().unwrap(), None);
    assert!(!child.exited().unwrap());
    child.stdin.as_mut().unwrap().write_all(b"x").unwrap();
    let deadline = Instant::now() + Duration::from_secs(60);
    while !child.exited().unwrap() {
        assert!(Instant::now() < deadline, "the child never exited");
        crate::deadline::poll_yield();
    }
    let pid = i32::try_from(child.id()).unwrap();
    // SAFETY: two integers; the pid is this test's own unreaped child.
    let held = unsafe { libc::kill(pid, 0) } == 0;
    assert!(held, "a child read as exited was reaped");
    let usage = child.wait_with_rusage().unwrap();
    assert!(child.exited().unwrap());
    assert_eq!(usage.status.code(), Some(7));
    assert!(usage.maxrss >= ALLOCATED as u64, "{usage:?}");
    assert!(usage.cpu > Duration::ZERO, "{usage:?}");
    assert_eq!(child.try_wait_with_rusage().unwrap(), Some(usage));
    assert_eq!(child.wait_with_rusage().unwrap(), usage);
    assert_eq!(child.wait().unwrap(), usage.status);
    assert_eq!(child.try_wait().unwrap(), Some(usage.status));
    child.kill().unwrap();

    let mut ordinary = command("/bin/sh").args(["-c", "exit 3"]).spawn().unwrap();
    assert_eq!(ordinary.wait().unwrap().code(), Some(3));
    assert!(
        ordinary.try_wait_with_rusage().is_err(),
        "a consumed status has no recoverable accounting"
    );
}

struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos());
        let dir =
            std::env::temp_dir().join(format!("spawnfd-{tag}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        Self(dir)
    }

    fn join(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn write_mode(path: &Path, body: &str, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).expect("fixture dir");
    }
    std::fs::write(path, body).expect("write fixture");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).expect("chmod");
}

#[test]
fn every_stdio_mode_round_trips() {
    let mut child = command("/bin/cat")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn cat");
    child
        .stdin
        .take()
        .expect("a piped stdin")
        .write_all(b"through a pipe\n")
        .expect("write stdin");
    let out = child.wait_with_output().expect("wait_with_output");
    assert!(out.status.success(), "{:?}", out.status);
    assert_eq!(
        out.stdout, b"through a pipe\n",
        "piped stdin to piped stdout"
    );
    assert!(
        out.stderr.is_empty(),
        "an unset stderr is inherited, not piped"
    );

    let out = command("/bin/sh")
        .args(["-c", "read line; echo \"read=$?\"; echo err >&2"])
        .output()
        .expect("output");
    assert_eq!(out.stdout, b"read=1\n", "output(): stdin is /dev/null");
    assert_eq!(out.stderr, b"err\n", "output(): stderr is captured");

    let out = command("/bin/sh")
        .args(["-c", "echo out; echo err >&2"])
        .stdout(Stdio::null())
        .output()
        .expect("output");
    assert!(out.stdout.is_empty(), "a null stdout captures nothing");
    assert_eq!(out.stderr, b"err\n");

    let scratch = Scratch::new("stdio");
    let (reader, mut writer) = std::io::pipe().expect("pipe");
    writer.write_all(b"fed\n").expect("write the pipe");
    drop(writer);
    let status = command("/bin/sh")
        .args(["-c", "read line; echo \"got:$line\"; echo oops >&2; exit 4"])
        .stdin(reader)
        .stdout(std::fs::File::create(scratch.join("out")).expect("out file"))
        .stderr(std::fs::File::create(scratch.join("err")).expect("err file"))
        .status()
        .expect("status");
    assert_eq!(status.code(), Some(4), "status(): the exit code");
    assert_eq!(
        std::fs::read_to_string(scratch.join("out")).expect("read out"),
        "got:fed\n",
        "a handed stdin and stdout"
    );
    assert_eq!(
        std::fs::read_to_string(scratch.join("err")).expect("read err"),
        "oops\n",
        "a handed stderr"
    );

    let out = command("/bin/sh")
        .args([
            "-c",
            "yes out | head -c 1000000 & yes err | head -c 1000000 >&2; wait",
        ])
        .output()
        .expect("output");
    assert!(out.status.success(), "{:?}", out.status);
    assert_eq!(
        (out.stdout.len(), out.stderr.len()),
        (1_000_000, 1_000_000),
        "both floods arrive whole"
    );
}

#[test]
fn a_descriptor_at_any_number_reaches_its_slot() {
    let floor: RawFd = 10_300;
    require_host_holds(floor, "");
    require_soft_limit(10_400, floor);
    let scratch = Scratch::new("high");
    let file = std::fs::File::create(scratch.join("out")).expect("out file");
    let high = park(OwnedFd::from(file), floor);
    assert!(
        high.as_raw_fd() >= floor,
        "PRECONDITION: parked at {floor}+"
    );
    let status = command("/bin/sh")
        .args(["-c", "echo placed"])
        .stdout(high)
        .status()
        .expect("status");
    assert!(status.success(), "{status:?}");
    assert_eq!(
        std::fs::read_to_string(scratch.join("out")).expect("read out"),
        "placed\n"
    );
}

/// The staged half of [`an_inherited_stdio_reaches_the_child_and_a_closed_one_is_null`].
#[cfg(target_vendor = "apple")]
fn staged_inherit() {
    unsafe extern "C" {
        fn close(fd: RawFd) -> i32;
    }
    let status = command("/bin/sh")
        .args(["-c", "echo STAGED inherited-stdout"])
        .status()
        .expect("status");
    assert!(status.success(), "{status:?}");
    let scratch = Scratch::new("closed");
    let out = std::fs::File::create(scratch.join("out")).expect("out file");
    // SAFETY: closes this staged process's own stderr, which nothing else uses.
    assert_eq!(unsafe { close(2) }, 0, "close stderr");
    let status = command("/bin/sh")
        .args(["-c", "echo x >&2 && echo stderr-open"])
        .stdout(out)
        .stderr(Stdio::inherit())
        .status()
        .expect("status");
    let saw = std::fs::read_to_string(scratch.join("out")).expect("read out");
    println!("STAGED closed-stderr status={status:?} saw={saw:?}");
    assert_eq!(saw, "stderr-open\n");
    println!("STAGED inherit ok");
}

/// **AN INHERITED STDIO IS THIS PROCESS'S, AND A CLOSED ONE IS `/dev/null`.** A
/// child whose stdout is inherited writes into this process's stdout; one
/// whose inherited stderr this process has CLOSED gets `/dev/null` there, so
/// its writes succeed and its first `open` does not land on 2.
#[cfg(target_vendor = "apple")]
#[test]
fn an_inherited_stdio_reaches_the_child_and_a_closed_one_is_null() {
    const NAME: &str =
        "spawnfd::command_tests::an_inherited_stdio_reaches_the_child_and_a_closed_one_is_null";
    if std::env::var(STAGE_ENV).is_ok() {
        staged_inherit();
        return;
    }
    let out = run_staged(NAME, "inherit");
    assert!(out.contains("STAGED inherited-stdout"), "{out}");
    assert!(out.contains("STAGED inherit ok"), "{out}");
}

/// The staged half of [`a_pipe_end_this_process_keeps_never_passes_for_its_closed_stdio`].
#[cfg(target_vendor = "apple")]
fn staged_closed_stdio() {
    unsafe extern "C" {
        fn close(fd: RawFd) -> i32;
        fn dup2(old: RawFd, new: RawFd) -> RawFd;
    }
    let scratch = Scratch::new("closed-stdio");
    let err_path = scratch.join("err");
    let err = std::fs::File::create(&err_path).expect("err file");
    let stdout = std::io::stdout().as_fd().try_clone_to_owned();
    let saved = park(stdout.expect("dup stdout"), 64);
    // SAFETY: closes this staged process's own stdin and stdout, which nothing else uses until
    // `dup2` puts stdout back below.
    assert_eq!(unsafe { close(0) }, 0, "close stdin");
    assert_eq!(unsafe { close(1) }, 0, "close stdout");

    let mut child = command("/bin/sh")
        .args([
            "-c",
            "{ t=$(/usr/bin/stat -f %HT <&9); } 9>&1; [ \"$t\" = 'Character Device' ] \
             || { echo stdout=not-null >&2; exit 3; }; \
             cat >/dev/null; echo stdin-ended >&2",
        ])
        .stdin(Stdio::piped())
        .stderr(err.try_clone().expect("dup err"))
        .spawn()
        .expect("spawn");
    let mine = child.stdin.as_ref().expect("a piped stdin").as_raw_fd();
    child
        .stdin
        .take()
        .expect("a piped stdin")
        .write_all(b"to the end\n")
        .expect("write stdin");
    let until = Instant::now() + Duration::from_secs(60);
    let status = loop {
        if let Some(status) = child.try_wait().expect("try_wait") {
            break status;
        }
        if Instant::now() > until {
            child.kill().expect("kill");
            panic!("the child never saw its stdin end: it holds our end of it");
        }
        std::thread::sleep(Duration::from_millis(10));
    };

    let out = command("/bin/sh")
        .args([
            "-c",
            "[ \"$(/usr/bin/stat -f %HT)\" = 'Character Device' ] && echo stdin=null || echo stdin=not-null",
        ])
        .stdin(Stdio::inherit())
        .output()
        .expect("output");

    // SAFETY: puts this process's stdout back on 1 (nothing holds 1 now: every end above was moved
    // off it, closed or dropped).
    assert_eq!(unsafe { dup2(saved.as_raw_fd(), 1) }, 1, "restore stdout");
    let said = std::fs::read_to_string(&err_path).expect("read err");
    println!("STAGED closed-stdio our-end={mine} status={status:?} err={said:?} out={out:?}");
    assert!(mine > 2, "our end of the pipe sat on {mine}");
    assert_eq!(status.code(), Some(0), "{said}");
    assert_eq!(said, "stdin-ended\n", "the child's stdout was /dev/null");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "stdin=null\n",
        "the child's stdin was /dev/null, not our end of its stdout"
    );
    println!("STAGED closed-stdio ok");
}

/// **A PIPE END THIS PROCESS KEEPS NEVER PASSES FOR ITS CLOSED STDIO.** In a
/// process that has closed its stdin and stdout, `io::pipe()` hands out 0
/// and 1 — so this process's end of a child's pipe lands there and, left
/// there, reads to the spawn as this process's own stdio, which an inherited
/// slot hands the child (a child given the write end of its own stdin as its
/// stdout waits for its stdin's end forever).
#[cfg(target_vendor = "apple")]
#[test]
fn a_pipe_end_this_process_keeps_never_passes_for_its_closed_stdio() {
    const NAME: &str =
        "spawnfd::command_tests::a_pipe_end_this_process_keeps_never_passes_for_its_closed_stdio";
    if std::env::var(STAGE_ENV).is_ok() {
        staged_closed_stdio();
        return;
    }
    let out = run_staged(NAME, "closed-stdio");
    assert!(out.contains("STAGED closed-stdio ok"), "{out}");
}

enum Env {
    Set(&'static str, String),
    Remove(&'static str),
    Clear,
}

struct Case {
    what: &'static str,
    program: String,
    args: Vec<String>,
    env: Vec<Env>,
    cwd: Option<PathBuf>,
}

fn case(what: &'static str, program: &str, args: &[&str]) -> Case {
    Case {
        what,
        program: program.to_string(),
        args: args.iter().map(|a| (*a).to_string()).collect(),
        env: Vec::new(),
        cwd: None,
    }
}

impl Case {
    fn env(mut self, edit: Env) -> Self {
        self.env.push(edit);
        self
    }

    fn cwd(mut self, dir: impl Into<PathBuf>) -> Self {
        self.cwd = Some(dir.into());
        self
    }
}

macro_rules! configured {
    ($new:expr, $case:expr) => {{
        let case: &Case = $case;
        let mut cmd = $new(&case.program);
        cmd.args(&case.args);
        for edit in &case.env {
            match edit {
                Env::Set(key, value) => {
                    cmd.env(key, value);
                }
                Env::Remove(key) => {
                    cmd.env_remove(key);
                }
                Env::Clear => {
                    cmd.env_clear();
                }
            }
        }
        if let Some(dir) = &case.cwd {
            cmd.current_dir(dir);
        }
        cmd
    }};
}

type Outcome = Result<(Option<i32>, String, String), (io::ErrorKind, Option<i32>)>;

fn outcome(run: io::Result<std::process::Output>) -> Outcome {
    run.map(|out| {
        (
            out.status.code(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    })
    .map_err(|e| (e.kind(), e.raw_os_error()))
}

/// A NATIVE-IMAGE SPAWN REFUSES WHAT THE KERNEL WILL NOT EXECUTE ([`super::NativeImage`]): an
/// executable file of Mach-O magic and nothing else fails the spawn with `ENOEXEC`, on every
/// platform. NEGATIVE CONTROL: by default macOS's spawn runs that file as a shell script, so a
/// child starts and exits with a code of its own.
#[test]
fn a_native_image_spawn_refuses_what_the_kernel_will_not_execute() {
    use std::os::unix::fs::PermissionsExt;
    let scratch = Scratch::new("native-image");
    let garbage = scratch.join("garbage");
    std::fs::write(&garbage, [0xcf, 0xfa, 0xed, 0xfe, 0, 0, 0, 0]).expect("write fixture");
    std::fs::set_permissions(&garbage, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let refused = command(&garbage)
        .native_image(true)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    assert_eq!(
        refused.err().and_then(|e| e.raw_os_error()),
        Some(libc::ENOEXEC)
    );
    #[cfg(target_vendor = "apple")]
    {
        let mut by_shell = command(&garbage)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("the shell runs it");
        assert!(by_shell.wait().expect("its exit").code().is_some());
    }
}

/// A NATIVE-IMAGE SPAWN REFUSES WHAT IT COULD NOT START AS ITS OWN IMAGE: a tethered command
/// (whose helper is what the spawn starts) and, on Linux, a bare name started in another
/// directory (searched before the fork, against this one). NEGATIVE CONTROL: the same commands
/// without the flag start.
#[test]
fn a_native_image_spawn_refuses_what_it_could_not_start_as_its_image() {
    let mut tethered = command("/usr/bin/true");
    tethered.tether();
    let refused = tethered.native_image(true).spawn();
    assert_eq!(
        refused.err().map(|e| e.kind()),
        Some(io::ErrorKind::Unsupported)
    );
    #[cfg(target_os = "linux")]
    {
        let refused = command("true").current_dir("/").native_image(true).spawn();
        assert_eq!(
            refused.err().map(|e| e.kind()),
            Some(io::ErrorKind::Unsupported)
        );
        let mut started = command("true").current_dir("/").spawn().expect("starts");
        assert!(started.wait().expect("its exit").success());
    }
    let mut started = tethered.native_image(false).spawn().expect("starts");
    assert!(started.wait().expect("its exit").success());
}

#[test]
fn programs_arguments_environments_and_directories_match_std() {
    let scratch = Scratch::new("parity");
    let dir = |name: &str| scratch.join(name).display().to_string();
    write_mode(
        &scratch.join("bin/tool"),
        "#!/bin/sh\necho \"tool:$1\"\n",
        0o755,
    );
    write_mode(
        &scratch.join("denied/tool"),
        "#!/bin/sh\necho denied\n",
        0o474,
    );
    write_mode(
        &scratch.join("script/noshebang"),
        "echo \"noshebang:$1\"\n",
        0o755,
    );
    write_mode(&scratch.join("plain"), "#!/bin/sh\necho plain\n", 0o644);
    std::fs::create_dir_all(scratch.join("dirs/tool")).expect("a directory named tool");
    write_mode(
        &scratch.join("locked/tool"),
        "#!/bin/sh\necho locked\n",
        0o755,
    );
    let lock = |mode| {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            scratch.join("locked"),
            std::fs::Permissions::from_mode(mode),
        )
        .expect("chmod the locked directory");
    };
    lock(0o000);
    let pwd_and_args = r#"pwd -P; printf '[%s]' "$0" "$@"; echo"#;
    let long_name = "x".repeat(1_100);
    let cases = [
        case(
            "argv: $0, an empty and a non-ASCII argument",
            "/bin/sh",
            &["-c", pwd_and_args, "zero", "one two", "", "ünï"],
        ),
        case("an unchanged environment", "/usr/bin/env", &[]),
        case("set, set again, remove", "/usr/bin/env", &[])
            .env(Env::Set("SPAWNFD_A", "1".into()))
            .env(Env::Set("SPAWNFD_A", "2".into()))
            .env(Env::Remove("HOME")),
        case("cleared, then set", "/usr/bin/env", &[])
            .env(Env::Clear)
            .env(Env::Set("ONLY", "this".into())),
        case("cleared, set, removed", "/usr/bin/env", &[])
            .env(Env::Clear)
            .env(Env::Set("ONLY", "this".into()))
            .env(Env::Remove("ONLY")),
        case("set after a clear survives it", "/usr/bin/env", &[])
            .env(Env::Set("EARLY", "gone".into()))
            .env(Env::Clear)
            .env(Env::Set("LATE", "kept".into())),
        case(
            "a bare name on the inherited PATH",
            "sh",
            &["-c", "echo found"],
        ),
        case("a bare name on the child's PATH", "tool", &["x"]).env(Env::Set(
            "PATH",
            format!("{}:{}", dir("missing"), dir("bin")),
        )),
        case("a candidate it may not run is passed over", "tool", &["x"]).env(Env::Set(
            "PATH",
            format!("{}:{}", dir("denied"), dir("bin")),
        )),
        case("the only candidate may not run", "tool", &["x"]).env(Env::Set("PATH", dir("denied"))),
        case("a directory named like the program", "tool", &["x"])
            .env(Env::Set("PATH", dir("dirs"))),
        case("a directory, then the program", "tool", &["x"])
            .env(Env::Set("PATH", format!("{}:{}", dir("dirs"), dir("bin")))),
        case("no candidate anywhere", "tool", &["x"]).env(Env::Set("PATH", dir("missing"))),
        case("no PATH: /usr/bin:/bin", "sh", &["-c", "echo default"]).env(Env::Remove("PATH")),
        case("cleared: /usr/bin:/bin", "sh", &["-c", "echo cleared"]).env(Env::Clear),
        case(
            "an empty PATH entry is the working directory",
            "tool",
            &["x"],
        )
        .env(Env::Set("PATH", ":".into()))
        .cwd(scratch.join("bin")),
        case(
            "a relative PATH entry, from the working directory",
            "tool",
            &["x"],
        )
        .env(Env::Set("PATH", "bin".into()))
        .cwd(&scratch.0),
        case(
            "a relative program, from the working directory",
            "bin/tool",
            &["x"],
        )
        .cwd(&scratch.0),
        case(
            "an absolute working directory",
            "/bin/sh",
            &["-c", pwd_and_args],
        )
        .cwd(&scratch.0),
        case(
            "a relative working directory",
            "/bin/sh",
            &["-c", pwd_and_args],
        )
        .cwd("src"),
        case(
            "a missing working directory",
            "/bin/sh",
            &["-c", "echo ran"],
        )
        .cwd(scratch.join("missing")),
        case("a missing program", "/nonexistent/spawnfd-probe", &[]),
        case(
            "not a directory on the way",
            "/etc/hosts/spawnfd-probe",
            &[],
        ),
        case(
            "an unsearchable directory on the way",
            &dir("locked/tool"),
            &[],
        ),
        case("an unsearchable PATH entry", "tool", &["x"]).env(Env::Set("PATH", dir("locked"))),
        case("a PATH entry that is a file", "tool", &["x"]).env(Env::Set("PATH", dir("plain"))),
        case("a file without an execute bit", &dir("plain"), &[]),
        case("a directory as the program", &dir("dirs"), &[]),
        case("a script with no #! line", &dir("script/noshebang"), &["y"]),
        case("a bare script with no #! line", "noshebang", &["y"])
            .env(Env::Set("PATH", dir("script"))),
        case(
            "a name too long for the PATH, which is this process's",
            &long_name,
            &[],
        ),
        case("a name too long for an edited PATH", &long_name, &[])
            .env(Env::Set("PATH", "/usr/bin:/bin".into())),
        case("an empty program", "", &[]),
        case("a NUL in the program", "/bin/sh\0x", &[]),
        case("a NUL in an argument", "/bin/sh", &["-c", "a\0b"]),
        case("a NUL in an environment value", "/usr/bin/env", &[])
            .env(Env::Set("SPAWNFD_NUL", "x\0y".into())),
        case(
            "a NUL in the working directory",
            "/bin/sh",
            &["-c", "echo ran"],
        )
        .cwd("a\0b"),
    ];
    let std_answers_two_ways =
        |what: &str| !cfg!(target_vendor = "apple") && what == "a script with no #! line";
    let translator = translator();
    let std_mute = |ours: &Outcome, theirs: &Outcome| {
        translator.is_some()
            && ours.is_err()
            && *theirs == Ok((Some(127), String::new(), String::new()))
    };
    let mut kinds = Vec::new();
    let mut mute = Vec::new();
    for row in &cases {
        let theirs = outcome(configured!(std::process::Command::new, row).output());
        let ours = outcome(configured!(command, row).output());
        if std_mute(&ours, &theirs) {
            mute.push(row.what);
        } else if !std_answers_two_ways(row.what) {
            assert_eq!(ours, theirs, "{}: ours, then std's", row.what);
        }
        kinds.push((row.what, ours));
    }
    if let Some(translator) = &translator
        && !mute.is_empty()
    {
        println!(
            "NOT RUN under {translator}: spawnfd::command_tests::\
             programs_arguments_environments_and_directories_match_std: std's answer to {} \
             of {} rows ({}): {STD_SPAWN_MUTE}",
            mute.len(),
            cases.len(),
            mute.join("; ")
        );
    }
    lock(0o755);
    let kind_of = |what: &str| {
        kinds
            .iter()
            .find(|(w, _)| *w == what)
            .map(|(_, o)| o.as_ref().map(|(code, ..)| *code).map_err(|(k, _)| *k))
    };
    assert_eq!(
        kind_of("a candidate it may not run is passed over"),
        Some(Ok(Some(0)))
    );
    assert_eq!(
        kind_of("the only candidate may not run"),
        Some(Err(io::ErrorKind::PermissionDenied))
    );
    assert_eq!(
        kind_of("a missing program"),
        Some(Err(io::ErrorKind::NotFound))
    );
    assert_eq!(
        kind_of("a missing working directory"),
        Some(Err(io::ErrorKind::NotFound))
    );
    assert_eq!(
        kind_of("a file without an execute bit"),
        Some(Err(io::ErrorKind::PermissionDenied))
    );
    assert_eq!(
        kind_of("a NUL in an argument"),
        Some(Err(io::ErrorKind::InvalidInput))
    );
    assert_eq!(
        kind_of("not a directory on the way"),
        Some(Err(io::ErrorKind::NotADirectory))
    );
    assert_eq!(
        kind_of("an unsearchable PATH entry"),
        Some(Err(if cfg!(target_vendor = "apple") {
            io::ErrorKind::NotFound
        } else {
            io::ErrorKind::PermissionDenied
        }))
    );
    assert_eq!(kind_of("a script with no #! line"), Some(Ok(Some(0))));
    if cfg!(target_vendor = "apple") {
        assert_eq!(
            kind_of("a name too long for the PATH, which is this process's"),
            Some(Err(io::ErrorKind::InvalidFilename))
        );
        assert_eq!(
            kind_of("a name too long for an edited PATH"),
            Some(Err(io::ErrorKind::NotFound))
        );
    }
}

#[test]
fn the_accessors_answer_what_the_command_was_given() {
    let mut ours = command("prog");
    ours.args(["a", "b"])
        .env("Z", "1")
        .env_remove("A")
        .current_dir("/tmp");
    let mut theirs = std::process::Command::new("prog");
    theirs
        .args(["a", "b"])
        .env("Z", "1")
        .env_remove("A")
        .current_dir("/tmp");
    assert_eq!(ours.get_program(), theirs.get_program());
    assert!(ours.get_args().eq(theirs.get_args()));
    assert!(ours.get_envs().eq(theirs.get_envs()));
    assert_eq!(ours.get_current_dir(), theirs.get_current_dir());
    assert_eq!(format!("{ours:?}"), format!("{theirs:?}"));
}

#[test]
fn arg0_is_the_childs_argv_zero_and_only_that() {
    use std::os::unix::process::CommandExt as _;
    let script = ["-c", "echo \"$0\""];
    let ours = command("sh")
        .arg0("renamed-sh")
        .args(script)
        .output()
        .expect("output");
    let theirs = std::process::Command::new("sh")
        .arg0("renamed-sh")
        .args(script)
        .output()
        .expect("output");
    assert_eq!(String::from_utf8_lossy(&ours.stdout), "renamed-sh\n");
    assert_eq!(ours.stdout, theirs.stdout, "ours, then std's");
    assert!(ours.status.success() && theirs.status.success());

    let mut ours = command("prog");
    ours.arg0("zero").arg("a");
    let mut theirs = std::process::Command::new("prog");
    theirs.arg0("zero").arg("a");
    assert_eq!(ours.get_program(), theirs.get_program());
    assert!(ours.get_args().eq(theirs.get_args()));
    assert_eq!(format!("{ours:?}"), format!("{theirs:?}"));
    let mut same = command("prog");
    same.arg0("prog");
    let mut theirs_same = std::process::Command::new("prog");
    theirs_same.arg0("prog");
    assert_eq!(format!("{same:?}"), format!("{theirs_same:?}"));
}

fn held_out_err(fd: RawFd) -> [String; 2] {
    [
        "-c".to_string(),
        format!(
            "{} && printf held || printf clear; printf ' out'; printf err >&2; exit 3",
            test_support::sh_held(fd)
        ),
    ]
}

#[test]
fn a_command_child_holds_its_stdio_and_its_pipes_and_nothing_else() {
    let held = unflagged_at_or_above(64);
    let fd = held.as_raw_fd();
    let args = held_out_err(fd);

    let control = test_support::leaky_output("/bin/sh", &args).expect("control");
    assert_eq!(
        String::from_utf8_lossy(&control.stdout),
        "held out",
        "CONTROL: a leaky spawn carries fd {fd}"
    );
    assert_eq!(control.status.code(), Some(3), "CONTROL: the probe ran");

    let mut child = command("/bin/sh")
        .args(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn");
    let mut got = String::new();
    child
        .stdout
        .take()
        .expect("a piped stdout")
        .read_to_string(&mut got)
        .expect("read stdout");
    assert_eq!(child.wait().expect("wait").code(), Some(3));
    assert_eq!(got, "clear out", "spawn: nothing inherited, and the pipe");

    let out = command("/bin/sh").args(&args).output().expect("output");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "clear out");
    assert_eq!(String::from_utf8_lossy(&out.stderr), "err");
    assert_eq!(out.status.code(), Some(3));

    let probe = test_support::held_probe(fd);
    let status = command("/bin/sh")
        .args(["-c", &probe])
        .status()
        .expect("status");
    assert_eq!(status.code(), Some(0), "status: fd {fd} reached the child");
    let by_hand = std::process::Command::new("/bin/sh")
        .inherit_nothing()
        .args(["-c", &probe])
        .status()
        .expect("status");
    assert_eq!(
        by_hand.code(),
        Some(0),
        "inherit_nothing: fd {fd} reached the child"
    );
    drop(held);
}

/// How many spawns the pair race runs through [`command`], and through a
/// deliberately leaky raw spawn for its control.
#[cfg(target_vendor = "apple")]
const PAIR_RACE_SPAWNS: usize = 400;
#[cfg(target_vendor = "apple")]
const PAIR_RACE_CONTROL_SPAWNS: usize = 200;

/// The staged half of the pair race. A thread makes `UnixStream::pair()`s as
/// fast as it can — Darwin has no `SOCK_CLOEXEC`, so each end is unflagged from
/// its `socketpair` to its `F_SETFD` — while this one spawns children that
/// report any descriptor at 3..=31 (a fresh staged table hands the pair ends
/// out from 3 up).
#[cfg(target_vendor = "apple")]
fn staged_pair_race() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let stop = std::sync::Arc::new(AtomicBool::new(false));
    let racer = {
        let stop = std::sync::Arc::clone(&stop);
        std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                let pair = std::os::unix::net::UnixStream::pair().expect("socketpair");
                drop(pair);
            }
        })
    };
    let probe = format!(
        "{} && exit 1; exit 0",
        test_support::sh_held_any(&(3..=31).collect::<Vec<_>>())
    );
    let leaks = (0..PAIR_RACE_SPAWNS)
        .filter(|_| {
            let status = command("/bin/sh")
                .args(["-c", &probe])
                .stdin(Stdio::null())
                .status()
                .expect("spawn");
            status.code() != Some(0)
        })
        .count();
    let control = (0..PAIR_RACE_CONTROL_SPAWNS)
        .filter(|_| {
            let status = test_support::leaky_status("/bin/sh", &["-c", &probe]).expect("spawn");
            status.code() != Some(0)
        })
        .count();
    stop.store(true, Ordering::Relaxed);
    racer.join().expect("racer");
    println!(
        "STAGED pair-race command={leaks}/{PAIR_RACE_SPAWNS} \
         leaky={control}/{PAIR_RACE_CONTROL_SPAWNS}"
    );
    assert!(
        control > 0,
        "CONTROL: no pair end ever reached a leaky spawn"
    );
    assert_eq!(leaks, 0, "a pair end reached a command() child");
}

/// **ZERO LEAKS UNDER A RACING `UnixStream::pair()`, OVER 400 SPAWNS.** A leaky
/// spawn racing another thread's socketpair carries a pair end into some of
/// its children (the control); [`command`] carries it into none.
#[cfg(target_vendor = "apple")]
#[test]
fn no_pair_end_reaches_a_command_child_while_another_thread_makes_pairs() {
    const NAME: &str = "spawnfd::command_tests::no_pair_end_reaches_a_command_child_while_another_thread_makes_pairs";
    if std::env::var(STAGE_ENV).is_ok() {
        staged_pair_race();
        return;
    }
    let out = run_staged(NAME, "pair-race");
    assert!(out.contains("STAGED pair-race"), "{out}");
    print!("{out}");
}

unsafe extern "C" {
    fn signal(sig: i32, handler: usize) -> usize;
    fn pthread_sigmask(how: i32, set: *const SigSet, old: *mut SigSet) -> i32;
}

/// `sigset_t`: a 32-bit mask on Darwin, 1024 bits on Linux.
#[cfg(target_vendor = "apple")]
type SigSet = u32;
#[cfg(not(target_vendor = "apple"))]
type SigSet = [u64; 16];

const SIGUSR1: i32 = if cfg!(target_vendor = "apple") {
    30
} else {
    10
};
const SIGUSR2: i32 = if cfg!(target_vendor = "apple") {
    31
} else {
    12
};
const SIGTERM: i32 = 15;
const SIG_BLOCK: i32 = if cfg!(target_vendor = "apple") { 1 } else { 0 };
const SIG_IGN: usize = 1;

fn sigset_of(signals: &[i32]) -> SigSet {
    #[cfg(target_vendor = "apple")]
    {
        signals.iter().fold(0, |set, &sig| set | (1 << (sig - 1)))
    }
    #[cfg(not(target_vendor = "apple"))]
    {
        let mut set = [0u64; 16];
        for &sig in signals {
            let bit = usize::try_from(sig - 1).expect("a signal number");
            set[bit / 64] |= 1 << (bit % 64);
        }
        set
    }
}

const MASK_PROBE: &str = "mask-probe";

fn probed_mask(out: &std::process::Output) -> String {
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines()
        .find_map(|line| line.split_once("MASK=").map(|(_, mask)| mask.trim()))
        .unwrap_or_else(|| panic!("the probe printed no mask: {text}"))
        .to_string()
}

macro_rules! mask_probe {
    ($new:expr) => {{
        let mut probe = $new(std::env::current_exe().expect("the test binary"));
        probe
            .args([
                "--exact",
                "spawnfd::command_tests::a_child_starts_with_no_signal_blocked_and_std_s_dispositions",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(STAGE_ENV, MASK_PROBE);
        probe
    }};
}

fn signal_mask_probe_stage_if_asked() -> bool {
    if std::env::var(STAGE_ENV).as_deref() != Ok(MASK_PROBE) {
        return false;
    }
    let mut mask = sigset_of(&[]);
    // SAFETY: a null `set` only reads this thread's mask into a live local.
    assert_eq!(
        unsafe { pthread_sigmask(SIG_BLOCK, std::ptr::null(), &mut mask) },
        0
    );
    println!("\nMASK={}", sigset_bits(&mask));
    true
}

fn sigset_bits(set: &SigSet) -> String {
    const SIGNALS: i32 = if cfg!(target_vendor = "apple") {
        32
    } else {
        64
    };
    (1..=SIGNALS)
        .filter(|&sig| {
            let probe = sigset_of(&[sig]);
            #[cfg(target_vendor = "apple")]
            {
                set & probe != 0
            }
            #[cfg(not(target_vendor = "apple"))]
            {
                set.iter().zip(probe.iter()).any(|(a, b)| a & b != 0)
            }
        })
        .map(|sig| sig.to_string())
        .collect::<Vec<_>>()
        .join(",")
}

fn staged_signals() {
    // SAFETY: `signal` with `SIG_IGN`, and a mask change for this thread; both take plain integers
    // or a live local.
    unsafe {
        signal(SIGUSR2, SIG_IGN);
        let block = sigset_of(&[SIGUSR1, SIGTERM]);
        assert_eq!(pthread_sigmask(SIG_BLOCK, &block, std::ptr::null_mut()), 0);
    }
    let ours_mask = probed_mask(&crate::deadline::ends_within(
        &mut mask_probe!(command),
        crate::deadline::ECHO_HANG,
    ));
    let theirs_mask = probed_mask(&crate::deadline::std_ends_within(
        &mut mask_probe!(std::process::Command::new),
        crate::deadline::ECHO_HANG,
    ));
    let mut ours = command("/bin/sleep")
        .arg("60")
        .spawn()
        .expect("spawn sleep");
    let pid = i32::try_from(ours.id()).expect("a pid");
    // SAFETY: our own unreaped child.
    assert_eq!(unsafe { kill(pid, SIGTERM) }, 0, "SIGTERM our child");
    let ended = ours.wait().expect("wait");
    println!("STAGED masks ours={ours_mask:?} std={theirs_mask:?} ended={ended:?}");
    let mut blocked = [SIGUSR1, SIGTERM];
    blocked.sort_unstable();
    assert_eq!(
        theirs_mask,
        format!("{},{}", blocked[0], blocked[1]),
        "CONTROL: a std child starts with the spawning thread's mask, so the probe can see one"
    );
    assert_eq!(ours_mask, "", "the child started with a signal blocked");
    assert_eq!(
        ended.signal(),
        Some(SIGTERM),
        "SIGTERM, blocked here, still ends the child"
    );
    let shell = "(yes; echo \"yes:$?\" >&2) | head -c1 >/dev/null; kill -USR2 $$; echo survived";
    let ours = command("/bin/sh")
        .args(["-c", shell])
        .output()
        .expect("output");
    let theirs = std::process::Command::new("/bin/sh")
        .args(["-c", shell])
        .output()
        .expect("output");
    let show = |o: &std::process::Output| {
        format!(
            "{}|{}",
            String::from_utf8_lossy(&o.stdout).trim(),
            String::from_utf8_lossy(&o.stderr).trim()
        )
    };
    println!(
        "STAGED dispositions ours={} std={}",
        show(&ours),
        show(&theirs)
    );
    assert_eq!(
        show(&ours),
        show(&theirs),
        "the dispositions std's child gets"
    );
    assert_eq!(show(&ours), "survived|yes:141");
    println!("STAGED signals ok");
}

#[test]
fn a_child_starts_with_no_signal_blocked_and_std_s_dispositions() {
    const NAME: &str =
        "spawnfd::command_tests::a_child_starts_with_no_signal_blocked_and_std_s_dispositions";
    if signal_mask_probe_stage_if_asked() {
        return;
    }
    if std::env::var(STAGE_ENV).is_ok() {
        staged_signals();
        return;
    }
    let out = run_staged(NAME, "signals");
    assert!(out.contains("STAGED signals ok"), "{out}");
    print!("{out}");
}

#[test]
fn the_child_handle_reaps_once_and_kills_only_its_own() {
    let mut done = command("/bin/sh")
        .args(["-c", "exit 5"])
        .spawn()
        .expect("spawn");
    let first = done.wait().expect("wait");
    assert_eq!(first.code(), Some(5));
    assert_eq!(
        done.wait().expect("second wait"),
        first,
        "the status is kept"
    );
    assert_eq!(done.try_wait().expect("try_wait"), Some(first));
    done.kill()
        .expect("kill after the reap is Ok and signals nothing");

    let mut running = command("/bin/sleep").arg("60").spawn().expect("spawn");
    assert!(running.id() > 0);
    assert_eq!(running.try_wait().expect("try_wait"), None, "still running");
    running.kill().expect("kill");
    let status = running.wait().expect("wait");
    assert_eq!(status.signal(), Some(SIGKILL), "{status:?}");

    let mut reader = command("/bin/cat")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .expect("spawn cat");
    reader
        .stdin
        .as_mut()
        .expect("a piped stdin")
        .write_all(b"to the end\n")
        .expect("write");
    assert!(
        reader.wait().expect("wait").success(),
        "wait closed the stdin it held"
    );
}

fn state_of(pid: u32) -> String {
    let out = std::process::Command::new("/bin/ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
        .expect("run ps");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn until_zombie(pid: u32) {
    let until = Instant::now() + Duration::from_secs(60);
    while !state_of(pid).starts_with('Z') {
        assert!(Instant::now() < until, "{pid} never exited");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn a_dropped_handle_of_an_exited_child_leaves_no_zombie() {
    let ours = command("/usr/bin/true").spawn().expect("spawn");
    let ours_pid = ours.id();
    until_zombie(ours_pid);
    drop(ours);
    let theirs = std::process::Command::new("/usr/bin/true")
        .spawn()
        .expect("spawn");
    let theirs_pid = theirs.id();
    until_zombie(theirs_pid);
    drop(theirs);
    let theirs_left = state_of(theirs_pid).starts_with('Z');
    let mut raw = 0;
    // SAFETY: blocking reap of the control, this test's own child.
    unsafe { waitpid(i32::try_from(theirs_pid).expect("pid"), &mut raw, 0) };
    // SAFETY: a non-blocking probe; `raw` is a live local.
    let ours_left = unsafe { waitpid(i32::try_from(ours_pid).expect("pid"), &mut raw, WNOHANG) };
    assert!(theirs_left, "CONTROL: a dropped std child stays a zombie");
    assert_eq!(
        ours_left, -1,
        "the dropped handle collected its exited child"
    );

    let (read, write) = std::io::pipe().expect("pipe");
    let running = command("/bin/sh")
        .args(["-c", "read _ || :"])
        .stdin(Stdio::from(read))
        .spawn()
        .expect("spawn running child");
    let running_pid = running.id();
    drop(running);
    drop(write);
    let until = Instant::now() + Duration::from_secs(60);
    while !state_of(running_pid).is_empty() {
        assert!(
            Instant::now() < until,
            "the dropped running child ({running_pid}) was never reaped"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    // SAFETY: a non-blocking probe; `raw` is a live local.
    let running_left =
        unsafe { waitpid(i32::try_from(running_pid).expect("pid"), &mut raw, WNOHANG) };
    assert_eq!(
        running_left, -1,
        "the reaper collected the child dropped while still running"
    );
}

/// **A CHILD STARTED SUSPENDED RUNS ONLY ONCE RESUMED.** `spawn` returns with the
/// child exec'd and stopped (`ps`: `T`) before its first instruction, so the shell has
/// not left its marker; [`Child::resume`] runs it to a clean exit that leaves it.
#[cfg(target_vendor = "apple")]
#[test]
fn a_child_started_suspended_runs_only_once_resumed() {
    let scratch = Scratch::new("suspended");
    let marker = scratch.join("ran");
    let script = format!("printf ran > '{}'", marker.display());
    let mut held = command("/bin/sh")
        .args(["-c", &script])
        .start_suspended(true)
        .spawn()
        .expect("spawn suspended");
    assert!(
        state_of(held.id()).starts_with('T'),
        "the child is stopped at its exec: {:?}",
        state_of(held.id())
    );
    assert!(!marker.exists(), "a stopped child ran");
    held.resume().expect("resume");
    assert!(held.wait().expect("wait").success());
    assert_eq!(std::fs::read_to_string(&marker).expect("the marker"), "ran");
    held.resume()
        .expect("resume after the reap is Ok and signals nothing");

    let unrun = scratch.join("unrun");
    let mut killed = command("/bin/sh")
        .args(["-c", &format!("printf ran > '{}'", unrun.display())])
        .start_suspended(true)
        .spawn()
        .expect("spawn suspended");
    killed.kill().expect("kill");
    let status = killed.wait().expect("wait");
    assert_eq!(status.signal(), Some(SIGKILL), "{status:?}");
    assert!(!unrun.exists(), "a held child killed unresumed ran");

    let mut control = command("/bin/sleep").arg("60").spawn().expect("spawn");
    let state = state_of(control.id());
    control.kill().expect("kill");
    let _ = control.wait();
    assert!(
        !state.starts_with('T'),
        "CONTROL: a plain child is not stopped: {state:?}"
    );
}

/// **A HELD CHILD DIES UNRUN WITH ITS HOLDER, EVEN ONE THAT IGNORES `SIGHUP`.** The
/// staged holder ignores `SIGHUP` (as `nohup` or a lane that sets `SIG_IGN` would), starts
/// a shell stopped in a group of its own, and exits without resuming or killing it.
#[cfg(target_vendor = "apple")]
#[test]
fn a_held_child_dies_unrun_with_its_holder_even_one_ignoring_hangups() {
    const NAME: &str =
        "spawnfd::command_tests::a_held_child_dies_unrun_with_its_holder_even_one_ignoring_hangups";
    const SIGHUP: i32 = 1;
    if let Ok(marker) = std::env::var(STAGE_ENV) {
        // SAFETY: `signal` with `SIG_IGN`, in this staged process of the test's own.
        unsafe { signal(SIGHUP, SIG_IGN) };
        let held = command("/bin/sh")
            .args(["-c", &format!("printf ran > '{marker}'")])
            .process_group(0)
            .start_suspended(true)
            .spawn()
            .expect("spawn suspended");
        println!("STAGED held {}", held.id());
        std::process::exit(0);
    }
    let scratch = Scratch::new("held-orphan");
    let marker = scratch.join("ran");
    let out = run_staged(NAME, marker.to_str().expect("utf-8 scratch path"));
    let pid: u32 = out
        .lines()
        .find_map(|line| line.split("STAGED held ").nth(1))
        .and_then(|pid| pid.trim().parse().ok())
        .unwrap_or_else(|| panic!("the stage named no held child: {out}"));
    let until = Instant::now() + Duration::from_secs(60);
    while !state_of(pid).is_empty() {
        assert!(
            Instant::now() < until,
            "the orphaned held child {pid} lives on"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(!marker.exists(), "a held child ran when its holder died");
}

#[test]
fn a_suspended_start_it_cannot_keep_starts_nothing() {
    let err = command("/nonexistent/spawnfd-suspended-exec")
        .start_suspended(true)
        .exec();
    assert_eq!(err.kind(), io::ErrorKind::Unsupported, "{err}");
    #[cfg(not(target_vendor = "apple"))]
    {
        let scratch = Scratch::new("suspended-refused");
        let marker = scratch.join("started");
        let mut cmd = command("/bin/sh");
        cmd.args(["-c", &format!("touch '{}'", marker.display())])
            .start_suspended(true);
        for (how, error) in [
            ("spawn", cmd.spawn().map(drop)),
            ("output", cmd.output().map(drop)),
            ("status", cmd.status().map(drop)),
        ] {
            let error = error.expect_err(how);
            assert_eq!(error.kind(), io::ErrorKind::Unsupported, "{how}: {error}");
        }
        assert!(!marker.exists(), "a refused suspended start ran the child");
    }
}

#[test]
fn a_child_leads_the_group_or_the_session_it_was_given() {
    unsafe extern "C" {
        fn getpgid(pid: i32) -> i32;
        fn getsid(pid: i32) -> i32;
        fn getpgrp() -> i32;
    }
    let mut grouped = command("/bin/sleep")
        .arg("60")
        .process_group(0)
        .spawn()
        .expect("spawn");
    let mut session = command("/bin/sleep")
        .arg("60")
        .setsid(true)
        .spawn()
        .expect("spawn");
    let mut plain = command("/bin/sleep").arg("60").spawn().expect("spawn");
    let mut both = command("/bin/sleep")
        .arg("60")
        .process_group(0)
        .setsid(true)
        .spawn()
        .expect("process_group(0) beside setsid");
    let pid = |c: &Child| i32::try_from(c.id()).expect("pid");
    // SAFETY: integer queries about this test's own unreaped children.
    let (group_pgid, session_sid, session_pgid, plain_pgid, ours, both_sid, both_pgid) = unsafe {
        (
            getpgid(pid(&grouped)),
            getsid(pid(&session)),
            getpgid(pid(&session)),
            getpgid(pid(&plain)),
            getpgrp(),
            getsid(pid(&both)),
            getpgid(pid(&both)),
        )
    };
    for child in [&mut grouped, &mut session, &mut plain, &mut both] {
        child.kill().expect("kill");
        child.wait().expect("wait");
    }
    assert_eq!(
        (both_sid, both_pgid),
        (pid(&both), pid(&both)),
        "process_group(0) beside setsid: the session's own group"
    );
    let mut joined = command("/bin/sleep")
        .arg("60")
        .process_group(ours)
        .setsid(true)
        .spawn()
        .expect("process_group(ours) beside setsid");
    // SAFETY: integer queries about this test's own unreaped child.
    let (joined_sid, joined_pgid) = unsafe { (getsid(pid(&joined)), getpgid(pid(&joined))) };
    joined.kill().expect("kill");
    joined.wait().expect("wait");
    assert_eq!(
        (joined_sid, joined_pgid),
        (pid(&joined), pid(&joined)),
        "process_group(ours) beside setsid: the session wins"
    );
    assert_eq!(
        group_pgid,
        pid(&grouped),
        "process_group(0): a group of its own"
    );
    assert_eq!(session_sid, pid(&session), "setsid: a session of its own");
    assert_eq!(session_pgid, pid(&session), "setsid: and its group");
    assert_eq!(plain_pgid, ours, "unasked, the child stays in this group");
}

#[test]
fn two_children_coexist_in_a_known_live_group_and_later_births_keep_their_own() {
    unsafe extern "C" {
        fn getpgid(pid: i32) -> i32;
        fn getpgrp() -> i32;
    }
    let mut leader = command("/bin/sleep")
        .arg("60")
        .spawn_in_dedicated_process_group()
        .expect("a self-led leader");
    let pid = |c: &Child| i32::try_from(c.id()).expect("pid");
    let group = pid(&leader);

    let mut first_command = command("/bin/sleep");
    first_command.arg("60");
    let mut first = first_command
        .spawn_in_process_group(group)
        .expect("the first member joins");
    let mut second_command = command("/bin/sleep");
    second_command.arg("60").process_group(0);
    let mut second = second_command
        .spawn_in_process_group(group)
        .expect("the second member joins");
    let mut first_later = first_command.spawn().expect("a later plain birth");
    let mut second_later = second_command.spawn().expect("a later configured birth");

    // SAFETY: integer queries about this test's own unreaped children.
    let (leader_pgid, first_pgid, second_pgid, first_later_pgid, second_later_pgid, ours) = unsafe {
        (
            getpgid(pid(&leader)),
            getpgid(pid(&first)),
            getpgid(pid(&second)),
            getpgid(pid(&first_later)),
            getpgid(pid(&second_later)),
            getpgrp(),
        )
    };
    let second_later_pid = pid(&second_later);
    for child in [
        &mut first,
        &mut second,
        &mut first_later,
        &mut second_later,
        &mut leader,
    ] {
        child.kill().expect("kill");
        child.wait().expect("wait");
    }

    assert_eq!(leader_pgid, group, "the leader leads the known group");
    assert_eq!(first_pgid, group, "the first member is in the group");
    assert_eq!(second_pgid, group, "the second member coexists in it");
    assert_eq!(
        first_later_pgid, ours,
        "a default command's later birth is back in this group"
    );
    assert_eq!(
        second_later_pgid, second_later_pid,
        "a configured command's later birth leads its own group again"
    );
}

#[test]
fn joining_a_missing_group_is_an_error_and_the_command_spawns_on_as_configured() {
    unsafe extern "C" {
        fn getpgid(pid: i32) -> i32;
    }
    let mut leader = command("/bin/sleep")
        .arg("60")
        .spawn_in_dedicated_process_group()
        .expect("a self-led leader");
    let pid = |c: &Child| i32::try_from(c.id()).expect("pid");
    let group = pid(&leader);
    leader.kill().expect("kill");
    leader.wait().expect("wait");

    let mut borrowed = command("/bin/sleep");
    borrowed.arg("60").process_group(0);
    let missing = borrowed.spawn_in_process_group(group);
    let missing_is_error = missing.is_err();
    if let Ok(mut child) = missing {
        child.kill().expect("kill");
        child.wait().expect("wait");
    }
    let mut later = borrowed.spawn().expect("a later configured birth");
    // SAFETY: an integer query about this test's own unreaped child.
    let later_pgid = unsafe { getpgid(pid(&later)) };
    let later_pid = pid(&later);
    later.kill().expect("kill");
    later.wait().expect("wait");

    assert!(missing_is_error, "joining a group that is gone is an error");
    assert_eq!(
        later_pgid, later_pid,
        "the failed join restored the configured group"
    );
}

#[test]
fn joining_a_group_refuses_contradictory_policies_before_the_target_runs() {
    unsafe extern "C" {
        fn getsid(pid: i32) -> i32;
        fn getpgid(pid: i32) -> i32;
    }
    let scratch = Scratch::new("join-refused");
    let mut leader = command("/bin/sleep")
        .arg("60")
        .spawn_in_dedicated_process_group()
        .expect("a self-led leader");
    let pid = |c: &Child| i32::try_from(c.id()).expect("pid");
    let group = pid(&leader);

    let attempt = |marker: &Path, join: &mut dyn FnMut(&mut Command) -> io::Result<Child>| {
        let mut cmd = command("/bin/sh");
        cmd.args(["-c", &format!("touch '{}'", marker.display())]);
        match join(&mut cmd) {
            Ok(mut child) => {
                let _ = child.wait();
                None
            }
            Err(error) => Some(error.kind()),
        }
    };

    let invalid_marker = scratch.join("invalid-started");
    let invalid = attempt(&invalid_marker, &mut |cmd| cmd.spawn_in_process_group(1));
    let session_marker = scratch.join("session-started");
    let session = attempt(&session_marker, &mut |cmd| {
        cmd.setsid(true);
        cmd.spawn_in_process_group(group)
    });
    let tether_marker = scratch.join("tether-started");
    let tethered = attempt(&tether_marker, &mut |cmd| {
        cmd.tether();
        cmd.spawn_in_process_group(group)
    });

    let mut session_command = command("/bin/sleep");
    session_command.arg("60").setsid(true);
    let refused = session_command.spawn_in_process_group(group);
    let refused_is_error = refused.is_err();
    if let Ok(mut child) = refused {
        child.kill().expect("kill");
        child.wait().expect("wait");
    }
    let mut session_child = session_command.spawn().expect("a later session birth");
    // SAFETY: integer queries about this test's own unreaped child.
    let (session_sid, session_pgid) =
        unsafe { (getsid(pid(&session_child)), getpgid(pid(&session_child))) };
    let session_pid = pid(&session_child);
    session_child.kill().expect("kill");
    session_child.wait().expect("wait");
    leader.kill().expect("kill");
    leader.wait().expect("wait");

    assert_eq!(invalid, Some(io::ErrorKind::InvalidInput));
    assert_eq!(session, Some(io::ErrorKind::InvalidInput));
    assert_eq!(tethered, Some(io::ErrorKind::InvalidInput));
    assert!(!invalid_marker.exists(), "an invalid join ran the target");
    assert!(!session_marker.exists(), "a refused session ran the target");
    assert!(!tether_marker.exists(), "a refused tether ran the target");
    assert!(refused_is_error, "a new session beside a join is refused");
    assert_eq!(
        (session_sid, session_pgid),
        (session_pid, session_pid),
        "the refusal left the session policy in place for a later birth"
    );
}

/// `responsibility_get_pid_responsible_for_pid(pid)`: the process macOS
/// charges `pid`'s TCC and syspolicyd questions to.
#[cfg(target_vendor = "apple")]
fn responsible_for(pid: u32) -> i32 {
    use core::ffi::{c_char, c_void};
    unsafe extern "C" {
        fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    }
    type Responsible = unsafe extern "C" fn(i32) -> i32;
    const RTLD_DEFAULT: *mut c_void = (-2_isize) as *mut c_void;
    // SAFETY: a NUL-terminated C string literal; `RTLD_DEFAULT` is dlfcn.h's pseudo-handle for a
    // global search.
    let found = unsafe {
        dlsym(
            RTLD_DEFAULT,
            c"responsibility_get_pid_responsible_for_pid".as_ptr(),
        )
    };
    assert!(
        !found.is_null(),
        "libSystem exports responsibility_get_pid_responsible_for_pid"
    );
    // SAFETY: dyld resolved the name to the libquarantine function whose C prototype is
    // `pid_t f(pid_t)`, exactly `Responsible`.
    let responsible = unsafe { std::mem::transmute::<*mut c_void, Responsible>(found) };
    // SAFETY: one plain integer argument.
    unsafe { responsible(i32::try_from(pid).expect("pid")) }
}

/// **A DISCLAIMED CHILD IS ITS OWN RESPONSIBLE PROCESS.** This test binary,
/// started through [`command`] with [`Command::disclaim_responsibility`], is a
/// STAGE that asks macOS who is responsible for whom: itself (its own pid);
/// a PLAIN `/bin/sleep` it starts (the stage's pid — the negative control,
/// true only if the stage really was disclaimed); and a DISCLAIMED
/// `/bin/sleep` it starts (that child's own pid, where its plain sibling
/// answered the stage's).
#[cfg(target_vendor = "apple")]
#[test]
fn a_disclaimed_child_is_its_own_responsible_process() {
    const NAME: &str = "spawnfd::command_tests::a_disclaimed_child_is_its_own_responsible_process";
    const STAGE: &str = "disclaim";
    if std::env::var(STAGE_ENV).as_deref() == Ok(STAGE) {
        let me = std::process::id();
        let of_me = responsible_for(me);
        let mut plain = command("/bin/sleep")
            .arg("60")
            .spawn()
            .expect("spawn a plain child");
        let mut disclaimed = command("/bin/sleep")
            .arg("60")
            .disclaim_responsibility(true)
            .spawn()
            .expect("spawn a disclaimed child");
        let (of_plain, of_disclaimed) = (
            responsible_for(plain.id()),
            responsible_for(disclaimed.id()),
        );
        let pid = |child: &Child| i32::try_from(child.id()).expect("pid");
        let (plain_pid, disclaimed_pid) = (pid(&plain), pid(&disclaimed));
        for child in [&mut plain, &mut disclaimed] {
            child.kill().expect("kill");
            child.wait().expect("wait");
        }
        let me = i32::try_from(me).expect("pid");
        println!(
            "STAGED disclaim stage={me}:{of_me} plain={plain_pid}:{of_plain} \
             disclaimed={disclaimed_pid}:{of_disclaimed}"
        );
        assert_eq!(
            of_me, me,
            "the stage, spawned disclaimed, is its own responsible process"
        );
        assert_eq!(
            of_plain, me,
            "CONTROL: a plain child is charged to its disclaimed parent"
        );
        assert_eq!(
            of_disclaimed, disclaimed_pid,
            "a disclaimed child is its own responsible process"
        );
        println!("STAGED disclaim ok");
        return;
    }
    let out = crate::deadline::ends_within(
        command(std::env::current_exe().expect("the test binary"))
            .args(["--exact", NAME, "--nocapture", "--test-threads=1"])
            .env(STAGE_ENV, STAGE)
            .disclaim_responsibility(true),
        crate::deadline::ECHO_HANG,
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && stdout.contains("STAGED disclaim ok"),
        "stage {STAGE}: {:?}\nstdout: {stdout}\nstderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The base priority the calling thread runs at once it asks for the default class (macOS's
/// `pth_priority`, read for this thread by its id): 31 outside a band, 4 in the background band.
#[cfg(target_vendor = "apple")]
fn base_priority_at_the_default_class() -> i32 {
    unsafe extern "C" {
        fn pthread_threadid_np(thread: *mut core::ffi::c_void, id: *mut u64) -> libc::c_int;
    }
    /// `<sys/proc_info.h>`'s by-thread-id flavor of `PROC_PIDTHREADINFO`.
    const PROC_PIDTHREADID64INFO: libc::c_int = 15;
    // SAFETY: sets this thread's own class; takes no pointers.
    let set =
        unsafe { libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_DEFAULT, 0) };
    assert_eq!(set, 0, "this thread asks for the default class");
    let mut id = 0u64;
    // SAFETY: a null thread names the calling one; `id` is a writable slot.
    assert_eq!(
        unsafe { pthread_threadid_np(std::ptr::null_mut(), &mut id) },
        0
    );
    let mut info = std::mem::MaybeUninit::<libc::proc_threadinfo>::uninit();
    let size = libc::c_int::try_from(size_of::<libc::proc_threadinfo>()).expect("a small record");
    // SAFETY: `info` is `size` writable bytes laid out as the record this flavor fills.
    let read = unsafe {
        libc::proc_pidinfo(
            libc::getpid(),
            PROC_PIDTHREADID64INFO,
            id,
            info.as_mut_ptr().cast(),
            size,
        )
    };
    assert_eq!(read, size, "this thread's record reads");
    // SAFETY: the exact-size read initialized every (integer) field.
    unsafe { info.assume_init() }.pth_priority
}

/// What a band READER (the calling test run again as its [`BAND_READER`] stage) said its thread
/// runs at once it asks for the default class ([`base_priority_at_the_default_class`]), `who`
/// naming it in a failure.
#[cfg(target_vendor = "apple")]
fn band_reader_reads(command: &mut Command, test: &str, who: &str) -> i32 {
    let out = crate::deadline::ends_within(
        command
            .args(["--exact", test, "--nocapture", "--test-threads=1"])
            .env(STAGE_ENV, BAND_READER),
        crate::deadline::ECHO_HANG,
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "the {who} reader: {:?}\nstdout: {stdout}\nstderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    stdout
        .lines()
        .find_map(|line| line.split_once(BAND_READ).map(|(_, said)| said))
        .and_then(|n| n.trim().parse().ok())
        .unwrap_or_else(|| panic!("the {who} reader said no priority: {stdout}"))
}

/// The stage a band test runs as a [`band_reader_reads`] reader.
#[cfg(target_vendor = "apple")]
const BAND_READER: &str = "band-read";

/// What a band reader prints before the priority it read.
#[cfg(target_vendor = "apple")]
const BAND_READ: &str = "BAND read default=";

/// The calling band test's stage, if this process is one: a reader prints its priority and
/// `true` comes back, the test's body done.
#[cfg(target_vendor = "apple")]
fn band_read_staged() -> bool {
    if std::env::var(STAGE_ENV).as_deref() == Ok(BAND_READER) {
        println!("{BAND_READ}{}", base_priority_at_the_default_class());
        return true;
    }
    false
}

/// What a reader this test starts outside any band reads (31; under a clamp the test runs in,
/// the clamp's, which a child keeps), which a band test compares with.
#[cfg(target_vendor = "apple")]
fn read_outside_any_band(test: &str) -> i32 {
    let exe = std::env::current_exe().expect("the test binary");
    let outside = band_reader_reads(&mut command(&exe), test, "outside");
    assert!(
        outside > 4,
        "a reader this test starts runs outside the background band (it read {outside}), as \
         this test needs to tell the band from outside it"
    );
    outside
}

/// A band stage's hang detector: the stage starts its two readers one after the other, each
/// under its own [`ECHO_HANG`](crate::deadline::ECHO_HANG), so its own covers both readers and
/// its own start. A stage held to one `ECHO_HANG` was cut off while its readers still ran, on a
/// Mac with all 14 cores busy (2026-11-04).
#[cfg(target_vendor = "apple")]
const BAND_STAGE_HANG: Duration = Duration::from_secs(3 * crate::deadline::ECHO_HANG.as_secs());

/// The stage `stage` of `test`, started by `command` (the test binary), and what its readers
/// read: the line after `STAGED band `.
#[cfg(target_vendor = "apple")]
fn band_stage_says(command: &mut Command, test: &str, stage: &str) -> String {
    let out = crate::deadline::ends_within(
        command
            .args(["--exact", test, "--nocapture", "--test-threads=1"])
            .env(STAGE_ENV, stage),
        BAND_STAGE_HANG,
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    stdout
        .lines()
        .find_map(|line| {
            line.split_once("STAGED band ")
                .map(|(_, said)| said.to_owned())
        })
        .unwrap_or_else(|| {
            panic!(
                "the stage said nothing: {:?}\nstdout: {stdout}\nstderr: {}",
                out.status,
                String::from_utf8_lossy(&out.stderr)
            )
        })
}

/// A stage's two readers, a PLAIN one and one started with [`Command::leave_background_band`],
/// printed as `STAGED band plain=<p> left=<p>`.
#[cfg(target_vendor = "apple")]
fn read_plain_and_left(test: &str) {
    let exe = std::env::current_exe().expect("the test binary");
    let plain = band_reader_reads(&mut command(&exe), test, "plain");
    let left = band_reader_reads(
        command(&exe).leave_background_band(true),
        test,
        "band-leaving",
    );
    println!("STAGED band plain={plain} left={left}");
}

/// **A CHILD STARTED OUT OF THE BACKGROUND BAND RUNS AT THE CLASS IT ASKS FOR.** This test
/// binary, started IN the band ([`Command::enter_background_band`], the task type launchd starts a
/// `Background` job as), is a STAGE that starts two READERS (this test again), each reading the
/// priority its thread runs at once it asks for the default class: a PLAIN one, which stays in the
/// band and reads 4 (the negative control, true only if the stage really was in it), and one
/// started with [`Command::leave_background_band`], which reads what a reader this test starts
/// outside any band reads ([`read_outside_any_band`]).
#[cfg(target_vendor = "apple")]
#[test]
fn a_child_started_out_of_the_background_band_runs_at_the_class_it_asks_for() {
    const NAME: &str = "spawnfd::command_tests::a_child_started_out_of_the_background_band_runs_at_the_class_it_asks_for";
    const STAGE: &str = "band";
    if band_read_staged() {
        return;
    }
    if std::env::var(STAGE_ENV).as_deref() == Ok(STAGE) {
        read_plain_and_left(NAME);
        return;
    }
    let outside = read_outside_any_band(NAME);
    let exe = std::env::current_exe().expect("the test binary");
    assert_eq!(
        band_stage_says(command(&exe).enter_background_band(), NAME, STAGE),
        format!("plain=4 left={outside}"),
        "CONTROL: a plain child of the band stays in it (4); a child started out of it reads \
         what one outside any band reads ({outside})"
    );
}

/// **A PROCESS THAT STARTS ITS CHILDREN IN THE BACKGROUND BAND STARTS EACH THERE, BUT ONE TOLD TO
/// LEAVE IT.** This test binary, started outside any band, is a STAGE that calls
/// [`start_children_in_background_band`] and starts two READERS: a PLAIN one, which reads 4, the
/// band's priority, and one started with [`Command::leave_background_band`], which reads what a
/// reader this test starts outside any band reads. NEGATIVE CONTROL: with the stage's call taken
/// out, its plain reader reads the outside value too, and this fails.
#[cfg(target_vendor = "apple")]
#[test]
fn a_process_that_starts_its_children_in_the_band_starts_each_there_but_one_told_to_leave() {
    const NAME: &str = "spawnfd::command_tests::a_process_that_starts_its_children_in_the_band_starts_each_there_but_one_told_to_leave";
    const STAGE: &str = "children";
    if band_read_staged() {
        return;
    }
    if std::env::var(STAGE_ENV).as_deref() == Ok(STAGE) {
        start_children_in_background_band();
        read_plain_and_left(NAME);
        return;
    }
    let outside = read_outside_any_band(NAME);
    let exe = std::env::current_exe().expect("the test binary");
    assert_eq!(
        band_stage_says(&mut command(&exe), NAME, STAGE),
        format!("plain=4 left={outside}"),
        "a plain child of a process that starts its children in the band runs there (4); one \
         told to leave it reads what one outside any band reads ({outside})"
    );
}

/// **A CHILD OF THE BACKGROUND BAND RUNS EVERY THREAD AT 4, AND ONE STARTED AS AN INTERACTIVE
/// DAEMON LEAVES THE BAND, READ FROM OUTSIDE EACH.** This test binary, started IN the band
/// ([`Command::enter_background_band`], the task type launchd starts a `Background` job as), is a
/// STAGE that starts two `cat` stand-ins, a PLAIN one and one with
/// [`Command::start_as_interactive_daemon`] (an `Interactive` job's), each shown to have run by a
/// line it echoed, and reads each one's threads by pid ([`crate::qos::thread_base_priorities`])
/// while they wait on their input. The plain one reads 4 on every thread, and the interactive
/// one's highest reads what a reader this test starts outside any band reads. NEGATIVE CONTROL:
/// the plain reading is true only if the stage was in the band, and it is what an interactive
/// builder that set no type would read, so either builder made a no-op fails; a reader that read
/// nothing of the child fails one of the two. A thread's priority does not tell an interactive
/// daemon from a standard one, so this shows the type leaves the band and no more.
#[cfg(target_vendor = "apple")]
#[test]
fn a_child_of_the_band_reads_4_and_one_started_as_an_interactive_daemon_leaves_it() {
    const NAME: &str = "spawnfd::command_tests::a_child_of_the_band_reads_4_and_one_started_as_an_interactive_daemon_leaves_it";
    const STAGE: &str = "interactive";
    if band_read_staged() {
        return;
    }
    if std::env::var(STAGE_ENV).as_deref() == Ok(STAGE) {
        let cat = crate::deadline::stand_in(Path::new("/bin/cat"));
        let read = |cmd: &mut Command, who: &str| -> Vec<i32> {
            let mut child = cmd
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()
                .unwrap_or_else(|e| panic!("start the {who} cat: {e}"));
            let input = crate::deadline::cat_echoes(&mut child);
            let pid = i32::try_from(child.id()).expect("a pid");
            let read = crate::qos::thread_base_priorities(pid);
            drop(input);
            let status = child.wait().expect("the cat's exit");
            assert!(status.success(), "the {who} cat: {status:?}");
            let mut read = read
                .filter(|p| !p.is_empty())
                .unwrap_or_else(|| panic!("the {who} cat's threads read nothing"));
            read.sort_unstable();
            read.dedup();
            read
        };
        let plain = read(&mut command(&cat), "plain");
        let interactive = read(command(&cat).start_as_interactive_daemon(), "interactive");
        println!(
            "STAGED band plain={plain:?} interactive={:?}",
            interactive.last()
        );
        return;
    }
    let outside = read_outside_any_band(NAME);
    let exe = std::env::current_exe().expect("the test binary");
    assert_eq!(
        band_stage_says(command(&exe).enter_background_band(), NAME, STAGE),
        format!("plain=[4] interactive=Some({outside})"),
        "CONTROL: every thread of a plain child of the band stays in it (4); the highest of one \
         started as an interactive daemon reads what one outside any band reads ({outside})"
    );
}

#[test]
fn a_disclaim_the_os_cannot_honour_starts_nothing() {
    const ABSENT: &core::ffi::CStr = c"responsibility_spawnattrs_setdisclaim_absent";
    let scratch = Scratch::new("disclaim-absent");
    let marker = scratch.join("started");
    let script = format!("touch '{}'", marker.display());
    let refused = |error: io::Error, how: &str| {
        assert_eq!(error.kind(), io::ErrorKind::Unsupported, "{how}: {error}");
        #[cfg(target_vendor = "apple")]
        assert!(
            error.to_string().contains(ABSENT.to_str().expect("utf-8")),
            "{how}: the refusal names the missing symbol: {error}"
        );
    };
    let mut cmd = command("/bin/sh");
    cmd.args(["-c", &script]).disclaim_through(ABSENT);
    refused(cmd.spawn().expect_err("spawn disclaimed"), "spawn");
    refused(cmd.output().expect_err("output disclaimed"), "output");
    refused(cmd.status().expect_err("status disclaimed"), "status");
    assert!(
        !marker.exists(),
        "a disclaim the OS could not apply started the child"
    );
    let control = command("/bin/sh")
        .args(["-c", &script])
        .status()
        .expect("CONTROL: the same command undisclaimed");
    assert!(control.success(), "CONTROL: {control:?}");
    assert!(
        marker.exists(),
        "CONTROL: the undisclaimed child left the marker"
    );
}

/// **A DISCLAIM IS DARWIN'S.** Elsewhere `spawn`, `output` and `status` answer
/// `Unsupported` and start nothing (`exec`: the staged test below).
#[cfg(not(target_vendor = "apple"))]
#[test]
fn a_disclaim_is_unsupported_off_darwin() {
    let mut cmd = command("/bin/true");
    cmd.disclaim_responsibility(true);
    for (how, error) in [
        ("spawn", cmd.spawn().map(drop)),
        ("output", cmd.output().map(drop)),
        ("status", cmd.status().map(drop)),
    ] {
        let error = error.expect_err(how);
        assert_eq!(error.kind(), io::ErrorKind::Unsupported, "{how}: {error}");
    }
}

fn staged_exec() {
    let held = unflagged_at_or_above(64);
    let fd = held.as_raw_fd();
    let failed = command("/nonexistent/spawnfd-exec-probe").exec();
    assert_eq!(failed.kind(), io::ErrorKind::NotFound, "{failed}");
    #[cfg(target_vendor = "apple")]
    assert_eq!(
        flags(fd) & FD_CLOEXEC,
        0,
        "a failed exec left this process's table as it was"
    );
    let err = command("/bin/sh")
        .args([
            "-c",
            &format!(
                "{} && echo 'STAGED exec held {fd}' && exit 3; echo 'STAGED exec ok'",
                test_support::sh_held(fd)
            ),
        ])
        .exec();
    panic!("exec /bin/sh: {err}");
}

#[test]
fn a_command_exec_replaces_this_process_holding_nothing() {
    const NAME: &str =
        "spawnfd::command_tests::a_command_exec_replaces_this_process_holding_nothing";
    if std::env::var(STAGE_ENV).is_ok() {
        staged_exec();
        return;
    }
    let out = run_staged(NAME, "exec");
    assert!(out.contains("STAGED exec ok"), "{out}");
}

#[test]
fn an_exec_that_must_disclaim_is_refused() {
    const NAME: &str = "spawnfd::command_tests::an_exec_that_must_disclaim_is_refused";
    if std::env::var(STAGE_ENV).is_ok() {
        let err = command("/bin/sh")
            .args(["-c", "echo 'STAGED exec-disclaim replaced'; exit 3"])
            .disclaim_responsibility(true)
            .exec();
        assert_eq!(err.kind(), io::ErrorKind::Unsupported, "{err}");
        println!("STAGED exec-disclaim refused");
        return;
    }
    let out = run_staged(NAME, "exec-disclaim");
    assert!(out.contains("STAGED exec-disclaim refused"), "{out}");
}

/// **AN `exec` HANDS OVER A CLOSE-ON-EXEC SOURCE AS `spawn` DOES.** Darwin's
/// `POSIX_SPAWN_SETEXEC` closes this process's close-on-exec descriptors
/// before it runs the file actions, so a `dup2` or an inherit FROM one answers
/// `EBADF` — and every descriptor `std` opens is close-on-exec.
#[cfg(target_vendor = "apple")]
#[test]
fn an_exec_hands_over_a_close_on_exec_source() {
    const NAME: &str = "spawnfd::command_tests::an_exec_hands_over_a_close_on_exec_source";
    if let Ok(path) = std::env::var(STAGE_ENV) {
        let file = std::fs::File::create(&path).expect("the stage's file");
        assert_ne!(
            flags(file.as_raw_fd()) & FD_CLOEXEC,
            0,
            "std's File is close-on-exec"
        );
        // SAFETY: sets one flag on this process's own stderr.
        unsafe { fcntl(2, F_SETFD, FD_CLOEXEC) };
        let err = command("/bin/sh")
            .args([
                "-c",
                &format!(
                    "echo wrote; {} && echo stderr-open; {}exit 0",
                    test_support::sh_held(2),
                    [5, 6, 7, 8, 9]
                        .map(|n| format!("{} && echo held:{n}; ", test_support::sh_held(n)))
                        .concat()
                ),
            ])
            .stdout(file)
            .exec();
        panic!("exec /bin/sh with a handed stdout: {err}");
    }
    let scratch = Scratch::new("exec-handed");
    let path = scratch.join("out");
    let _ = run_staged(NAME, path.to_str().expect("utf-8 scratch path"));
    let wrote = std::fs::read_to_string(&path).expect("the new image's stdout");
    assert_eq!(wrote, "wrote\nstderr-open\n");
}

/// A program still open for writing is tried again, not refused at once: Linux
/// refuses its exec (`ETXTBSY`) while anything holds it open for writing — a child another
/// thread forked while it was being written holds that descriptor until its own exec — and
/// the spawn tries again after the pause between tries, here the moment this test's writer
/// lets go. A tethered spawn forks each try on a spawner and pauses on the calling thread, so
/// it starts the same way.
#[cfg(target_os = "linux")]
#[test]
fn a_spawn_refused_as_text_busy_starts_once_the_writer_lets_go() {
    for tethered in [false, true] {
        starts_once_the_writer_lets_go(tethered);
    }
}

/// [`a_spawn_refused_as_text_busy_starts_once_the_writer_lets_go`], its spawns `tethered` or not.
#[cfg(target_os = "linux")]
fn starts_once_the_writer_lets_go(tethered: bool) {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = aterm_tempfile::tempdir().expect("scratch dir");
    let path = dir.path().join("busy");
    let mut writer = std::fs::File::create(&path).expect("create");
    writer.write_all(b"#!/bin/sh\nexit 7\n").expect("write");
    writer
        .set_permissions(std::fs::Permissions::from_mode(0o755))
        .expect("chmod");
    let tried_again = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let seen = std::sync::Arc::clone(&tried_again);
    TEXT_BUSY_HOOK.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            seen.store(true, std::sync::atomic::Ordering::SeqCst);
            drop(writer);
        }));
    });
    let busy = || {
        let mut busy = command(&path);
        if tethered {
            busy.tether();
        }
        busy
    };
    let status = busy()
        .status()
        .unwrap_or_else(|e| panic!("started once the writer let go (tethered: {tethered}): {e}"));
    assert_eq!(status.code(), Some(7), "tethered: {tethered}");
    assert!(
        tried_again.load(std::sync::atomic::Ordering::SeqCst),
        "the first try was refused and the spawn paused (tethered: {tethered})"
    );
    let _stays = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .expect("a writer that stays");
    let refused = busy().status().expect_err("refused once the window passed");
    assert_eq!(
        refused.kind(),
        io::ErrorKind::ExecutableFileBusy,
        "tethered: {tethered}: {refused}"
    );
}

/// What a tethered stage says of its child once the thread that spawned it is gone, after this
/// mark: `alive <token>` or `gone`; and `exec` from the image an exec put in a stage's place.
#[cfg(target_os = "linux")]
const TIED_SAID: &str = "STAGED tied ";

/// A group a stage of this test leads, with the stage, its leader, held unreaped until the group
/// holds no live process: the unreaped leader keeps its pid, and with it the group's number, from
/// being reused, so a signal to the group reaches the stage's own. Dropped with its leader
/// unreaped, it `SIGKILL`s the group and reaps the leader, so a failing run leaves nothing the
/// stage started.
#[cfg(target_os = "linux")]
struct StageGroup {
    leader: Child,
    reaped: bool,
}

#[cfg(target_os = "linux")]
impl StageGroup {
    /// `SIGKILL` the leader alone, leaving it unreaped.
    fn kill_leader(&mut self) {
        self.leader.kill().expect("SIGKILL the stage");
    }

    /// Waits until the group holds no live process ([`crate::process::group_members`], which
    /// skips a zombie: the unreaped leader's, or an orphan's its new parent has not reaped), then
    /// reaps the leader and answers how it ended; `None` when a live member was still there after
    /// `hang`.
    fn empties_within(&mut self, hang: Duration) -> Option<std::process::ExitStatus> {
        let pgid = self.leader.id();
        let since = Instant::now();
        while !crate::process::group_members(pgid).is_some_and(|members| members.pids.is_empty()) {
            if since.elapsed() >= hang {
                return None;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        self.reaped = true;
        Some(self.leader.wait().expect("reap the stage"))
    }
}

#[cfg(target_os = "linux")]
impl Drop for StageGroup {
    fn drop(&mut self) {
        if !self.reaped {
            // The leader is unreaped, so the group's number is still its own.
            if let Ok(pgid) = i32::try_from(self.leader.id()) {
                let _ = crate::process::kill_group_until_empty(pgid);
            }
            let _ = self.leader.wait();
        }
    }
}

/// The stage: spawn a tethered `sh` from a thread, join it, wait until the kernel has let that
/// thread's task go (its exit sends the parent-death signal first, and `join` can return before
/// it), then ask the `sh` for a line. `alive <token>` when it answered, else `gone`.
#[cfg(target_os = "linux")]
fn tied_child_after_its_thread(on_its_thread: bool, hang: Duration) -> String {
    use std::io::BufRead as _;
    let spawning = std::thread::spawn(move || {
        // SAFETY: `gettid` takes no argument and cannot fail.
        let tid = unsafe { libc::syscall(libc::SYS_gettid) };
        let mut sh = command("/bin/sh");
        sh.args(["-c", "read line; echo got; exec sleep 600"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .tether();
        if on_its_thread {
            sh.tie_to_this_thread();
        }
        (tid, sh.spawn().expect("spawn the tethered sh"))
    });
    let (tid, mut child) = spawning.join().expect("the spawning thread");
    let task = PathBuf::from(format!("/proc/self/task/{tid}"));
    let since = Instant::now();
    while task.exists() {
        assert!(
            since.elapsed() < hang,
            "the spawning thread's task {} never went",
            task.display()
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    let pid = i32::try_from(child.id()).expect("a pid");
    let mut stdin = child.stdin.take().expect("its stdin");
    let mut stdout = std::io::BufReader::new(child.stdout.take().expect("its stdout"));
    // A child that is gone refuses the line (`EPIPE`) and its stdout reads end-of-file.
    let _ = stdin.write_all(b"go\n");
    let mut line = String::new();
    let _ = stdout.read_line(&mut line);
    if line != "got\n" {
        return "gone".to_owned();
    }
    let identity = crate::process::read_identity(pid).expect("the child that answered reads");
    format!("alive {}", identity.token())
}

/// The stage's lifeline: the parent holds its stdin, so it reads end-of-file once the parent is
/// gone, however that ended.
#[cfg(target_os = "linux")]
fn until_the_parent_is_gone() {
    let _ = std::io::stdin().read_to_end(&mut Vec::new());
}

/// This test binary run as the stage `stage` of test `name`, in a group of its own, its stdin
/// held here as its lifeline, and each word it says after [`TIED_SAID`].
#[cfg(target_os = "linux")]
fn tied_stage(name: &str, stage: &str) -> (StageGroup, std::sync::mpsc::Receiver<String>) {
    use std::io::BufRead as _;
    let mut leader = command(std::env::current_exe().expect("the test binary"))
        .args(["--exact", name, "--nocapture", "--test-threads=1"])
        .env(STAGE_ENV, stage)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .process_group(0)
        .spawn()
        .expect("re-run this test binary as the stage");
    let out = std::io::BufReader::new(leader.stdout.take().expect("the stage's stdout"));
    let (said, words) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in out.lines().map_while(Result::ok) {
            if let Some((_, word)) = line.split_once(TIED_SAID)
                && said.send(word.to_owned()).is_err()
            {
                return;
            }
        }
    });
    (
        StageGroup {
            leader,
            reaped: false,
        },
        words,
    )
}

/// The token after `alive ` in `word`, a stage's word of its child.
#[cfg(target_os = "linux")]
fn alive(word: &str) -> Option<crate::process::Identity> {
    word.strip_prefix("alive ")
        .and_then(crate::process::Identity::parse)
}

/// Waits until `identity`'s pid no longer reads it, within `hang`; `what` says what should have
/// ended it.
#[cfg(target_os = "linux")]
fn ends_within(identity: crate::process::Identity, hang: Duration, what: &str) {
    let since = Instant::now();
    while crate::process::read_identity(identity.pid) == Some(identity) {
        assert!(
            since.elapsed() < hang,
            "the tethered child {} outlived {what}",
            identity.token()
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// On Linux a tethered child outlives the thread that spawned it, since its fork ran on one of
/// the tether's spawners, and ends with its process: it still runs while its stage, the process
/// that spawned it, does, and once the stage is `SIGKILL`ed its pid no longer reads its token.
/// The control ties the child to its spawning thread (`tie_to_this_thread`), and it is gone once
/// that thread is.
#[cfg(target_os = "linux")]
#[test]
fn a_linux_child_outlives_its_spawning_thread_but_not_its_process() {
    const NAME: &str =
        "spawnfd::command_tests::a_linux_child_outlives_its_spawning_thread_but_not_its_process";
    const ON_THE_SPAWNER: &str = "tied-on-the-spawner";
    const ON_ITS_THREAD: &str = "tied-on-its-thread";
    let hang = Duration::from_secs(60);
    match std::env::var(STAGE_ENV).as_deref() {
        Ok(ON_THE_SPAWNER) => {
            println!("{TIED_SAID}{}", tied_child_after_its_thread(false, hang));
            until_the_parent_is_gone();
            return;
        }
        Ok(ON_ITS_THREAD) => {
            println!("{TIED_SAID}{}", tied_child_after_its_thread(true, hang));
            return;
        }
        Ok(_) => return,
        Err(_) => {}
    }

    let (mut group, words) = tied_stage(NAME, ON_THE_SPAWNER);
    let word = words
        .recv_timeout(hang)
        .expect("the stage said nothing of its child in a minute");
    let Some(identity) = alive(&word) else {
        panic!("a child forked on the spawner is alive after its spawning thread: {word:?}");
    };
    assert_eq!(
        crate::process::read_identity(identity.pid),
        Some(identity),
        "the tethered child still runs while its process does"
    );
    group.kill_leader();
    ends_within(identity, hang, "its SIGKILLed process");
    assert!(
        group.empties_within(hang).is_some(),
        "the stage's group still holds a process"
    );

    let (mut group, words) = tied_stage(NAME, ON_ITS_THREAD);
    let word = words
        .recv_timeout(hang)
        .expect("the control stage said nothing of its child in a minute");
    assert_eq!(
        word, "gone",
        "CONTROL: a child tied to its spawning thread is gone once that thread is"
    );
    let status = group
        .empties_within(hang)
        .expect("the control stage's group still holds a process");
    assert!(status.success(), "the control stage: {status:?}");
}

/// On Linux an exec in a process's place ends its tethered children: the exec ends every other
/// thread, the spawner that forked each child among them, and with it sends the parent-death
/// signal. The stage says its child still runs just before its exec; the image the exec put in
/// its place says `exec`, and the child's pid then stops reading its token.
#[cfg(target_os = "linux")]
#[test]
fn a_linux_exec_in_place_ends_a_tethered_child() {
    const NAME: &str = "spawnfd::command_tests::a_linux_exec_in_place_ends_a_tethered_child";
    const ACROSS_AN_EXEC: &str = "tied-across-an-exec";
    let hang = Duration::from_secs(60);
    match std::env::var(STAGE_ENV).as_deref() {
        Ok(ACROSS_AN_EXEC) => {
            let word = tied_child_after_its_thread(false, hang);
            let runs = alive(&word).is_some_and(|identity| {
                crate::process::read_identity(identity.pid) == Some(identity)
            });
            println!("{TIED_SAID}{}", if runs { word.as_str() } else { "gone" });
            let replaced = format!("echo '{TIED_SAID}exec'; read line");
            let error = command("/bin/sh").args(["-c", &replaced]).exec();
            panic!("the exec in the stage's place: {error}");
        }
        Ok(_) => return,
        Err(_) => {}
    }

    let (mut group, words) = tied_stage(NAME, ACROSS_AN_EXEC);
    let word = words
        .recv_timeout(hang)
        .expect("the stage said nothing of its child in a minute");
    let Some(identity) = alive(&word) else {
        panic!("a tethered child runs until its process's exec: {word:?}");
    };
    let word = words
        .recv_timeout(hang)
        .expect("the image the exec put in the stage's place said nothing in a minute");
    assert_eq!(word, "exec", "the image the exec put in the stage's place");
    ends_within(identity, hang, "its process's exec");
    group.kill_leader();
    assert!(
        group.empties_within(hang).is_some(),
        "the stage's group still holds a process"
    );
}
