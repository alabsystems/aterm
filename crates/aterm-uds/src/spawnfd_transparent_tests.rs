// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for [`transparent`] descriptor inheritance, error parity, and signal/stdio behavior against `std::process::Command`.

use super::tests::{
    STAGE_ENV, STD_SPAWN_MUTE, park, run_staged_hup_ignored, translator, unflagged_at_or_above,
};
use super::*;
use std::path::PathBuf;

/// Decodes [`crate::host::held_probe`]'s exit status (`3` for held, `0` for clear).
fn answer(status: ExitStatus) -> bool {
    match status.code() {
        Some(3) => true,
        Some(0) => false,
        _ => panic!("the probe did not answer: {status:?}"),
    }
}

/// The shell that exits 3 when it holds `fd`, else 0.
fn holds(fd: RawFd) -> [String; 2] {
    ["-c".to_string(), test_support::held_probe(fd)]
}

#[test]
fn a_configurable_transparent_command_retains_descriptor_inheritance() {
    let unflagged = unflagged_at_or_above(100);
    let flagged = park(std::fs::File::open("/dev/null").unwrap().into(), 100);
    for (fd, expected) in [(unflagged.as_raw_fd(), true), (flagged.as_raw_fd(), false)] {
        let mut pass = transparent("/bin/sh");
        pass.args(holds(fd));
        let mut configured = pass.into_command();
        configured.stdin(Stdio::null());
        assert_eq!(answer(configured.status().unwrap()), expected);
        assert_eq!(
            answer(configured.status().unwrap()),
            expected,
            "reused command"
        );
    }
    assert!(!answer(
        command("/bin/sh")
            .args(holds(unflagged.as_raw_fd()))
            .status()
            .unwrap()
    ));
}

#[test]
fn a_configurable_transparent_command_retains_edits_and_accepts_owned_stdio() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut pass = transparent("/bin/sh");
    pass.arg0("native-configured").args([
        "-c",
        "printf '%s|%s|%s' \"$0\" \"$ATERM_CONFIGURED_PASS\" \"$PWD\"",
    ]);
    pass.env("ATERM_CONFIGURED_PASS", "first")
        .env("ATERM_CONFIGURED_PASS", "last");
    let result = pass
        .into_command()
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(result.status.success());
    let expected = format!(
        "native-configured|last|{}",
        std::fs::canonicalize(dir.path()).unwrap().display()
    );
    assert_eq!(String::from_utf8(result.stdout).unwrap(), expected);
}

#[test]
fn a_configurable_transparent_command_supplies_an_actual_cohort_child() {
    let mut command = transparent("/bin/cat").into_command();
    command.stdin(Stdio::piped()).stdout(Stdio::piped());
    let attempt = BirthAttempt::new().unwrap();
    let key = attempt.key().clone();
    let mut born = command.spawn_cohort(attempt).unwrap();
    let input = crate::deadline::cat_echoes(born.child_mut());
    assert_eq!(born.key(), &key);
    let pid = born.child_mut().id();
    assert_eq!(
        crate::process::read_lineage(i32::try_from(pid).unwrap())
            .unwrap()
            .pgid,
        pid
    );
    drop(input);
    assert!(born.into_child().wait().unwrap().success());
}

/// A [`transparent`] child inherits unflagged descriptors above stdio and excludes `FD_CLOEXEC` ones, whereas [`command`] and [`spawn`] inherit only stdio.
#[test]
fn a_transparent_child_holds_every_descriptor_this_process_hands_on_and_nothing_flagged() {
    let unflagged = unflagged_at_or_above(100);
    let flagged = park(
        std::fs::File::open("/dev/null")
            .expect("open /dev/null")
            .into(),
        100,
    );
    let (fd, home) = (unflagged.as_raw_fd(), flagged.as_raw_fd());
    assert!(fd >= 100 && home >= 100);

    let status = |program: &str, fd| {
        transparent(program)
            .args(holds(fd))
            .status()
            .expect("transparent status")
    };
    assert!(answer(status("/bin/sh", fd)), "fd {fd}: by path");
    assert!(answer(status("sh", fd)), "fd {fd}: searched on PATH");
    let mut child = transparent("/bin/sh")
        .args(holds(fd))
        .spawn()
        .expect("transparent spawn");
    assert!(child.id() > 0);
    assert!(answer(child.wait().expect("wait")), "fd {fd}: spawn");
    assert!(
        !answer(status("/bin/sh", home)),
        "fd {home}: close-on-exec, it stays home"
    );

    assert!(
        !answer(command("/bin/sh").args(holds(fd)).status().expect("status")),
        "command: fd {fd} reached the child"
    );
    let mut lane = LaneCommand::new("/bin/sh");
    lane.args(holds(fd));
    assert!(
        !answer(spawn(lane).expect("spawn").wait().expect("wait")),
        "spawn: fd {fd} reached the child"
    );
    drop((unflagged, flagged));
}

/// [`transparent`] returns the same `io::ErrorKind` as `std::process::Command` on missing, non-executable, or NUL-containing program names.
#[test]
fn a_transparent_spawn_fails_as_std_fails() {
    let translator = translator();
    let mut mute = Vec::new();
    for program in [
        "/nonexistent/aterm-transparent",
        "aterm-transparent-no-such-program",
        "/",
        "nul\0byte",
    ] {
        let ours = transparent(program).status().map_err(|e| e.kind());
        let theirs = std::process::Command::new(program)
            .status()
            .map_err(|e| e.kind());
        assert!(ours.is_err(), "{program:?}: {ours:?}");
        if translator.is_some() && theirs.is_ok_and(|status| status.code() == Some(127)) {
            mute.push(format!("{program:?}"));
            continue;
        }
        assert_eq!(ours, theirs, "{program:?}");
    }
    if let Some(translator) = translator
        && !mute.is_empty()
    {
        println!(
            "NOT RUN under {translator}: spawnfd::transparent_tests::\
             a_transparent_spawn_fails_as_std_fails: std's error for {}: {STD_SPAWN_MUTE}",
            mute.join(", ")
        );
    }
    let mut command = transparent("prog");
    command.arg("a").args(["b", "c"]);
    assert_eq!(command.get_program(), "prog");
    assert!(command.get_args().eq(["a", "b", "c"]));
}

/// Stage name for [`a_transparent_run_is_the_std_run_it_replaces`].
const SIGNALS_STAGE: &str = "transparent-signals";

/// Stage name for the mask-probe child spawned inside [`staged_signals`].
const MASK_PROBE_STAGE: &str = "transparent-mask-probe";

/// Prints the process's initial signal mask as `MASK=<signals>` when [`STAGE_ENV`] is [`MASK_PROBE_STAGE`].
fn transparent_mask_probe_stage_if_asked() -> bool {
    if std::env::var(STAGE_ENV).as_deref() != Ok(MASK_PROBE_STAGE) {
        return false;
    }
    println!("\nMASK={}", blocked());
    true
}

/// Stage name for the stdio-probe child spawned inside [`staged_signals`].
const STDIO_PROBE_STAGE: &str = "transparent-stdio-probe";

/// Prints the `(device, inode)` of descriptors 7, 8 and 9 as `STDIO=…` when
/// [`STAGE_ENV`] is [`STDIO_PROBE_STAGE`]: the staged shell dup'd its stdio there before
/// its own stdout was turned to the output file. `fstat` names nothing — a shell's
/// `stat` resolves device names (`devname`), which scans `/dev` and hangs on one wedged
/// node, and the child here is this binary, which starts without naming any terminal.
fn transparent_stdio_probe_stage_if_asked() -> bool {
    if std::env::var(STAGE_ENV).as_deref() != Ok(STDIO_PROBE_STAGE) {
        return false;
    }
    let ids = [7, 8, 9].map(|fd| {
        // SAFETY: `stat` is plain integers; all-zero is valid, and `fstat` fails cleanly
        // on a descriptor this process does not hold.
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        if unsafe { libc::fstat(fd, &raw mut st) } == 0 {
            format!("{}:{}", st.st_dev, st.st_ino)
        } else {
            "closed".to_string()
        }
    });
    println!("\nSTDIO={}", ids.join(","));
    true
}

/// `sigset_t`: a 32-bit mask on Darwin, 1024 bits on Linux.
#[cfg(target_vendor = "apple")]
type SigSet = u32;
#[cfg(not(target_vendor = "apple"))]
type SigSet = [u64; 16];

const SIGHUP: i32 = 1;
const SIGTERM: i32 = 15;
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
const SIG_BLOCK: i32 = if cfg!(target_vendor = "apple") { 1 } else { 0 };
const SIG_DFL: usize = 0;
const SIG_IGN: usize = 1;

unsafe extern "C" {
    fn signal(sig: i32, handler: usize) -> usize;
    fn pthread_sigmask(how: i32, set: *const SigSet, old: *mut SigSet) -> i32;
}

/// Whether signal `sig` is in `set`.
fn has(set: &SigSet, sig: i32) -> bool {
    let bit = u32::try_from(sig - 1).expect("a signal number");
    #[cfg(target_vendor = "apple")]
    {
        set & (1 << bit) != 0
    }
    #[cfg(not(target_vendor = "apple"))]
    {
        set[usize::try_from(bit / 64).expect("a word")] & (1 << (bit % 64)) != 0
    }
}

/// A set holding `signals`.
fn set_of(signals: &[i32]) -> SigSet {
    #[cfg(target_vendor = "apple")]
    let mut set: SigSet = 0;
    #[cfg(not(target_vendor = "apple"))]
    let mut set: SigSet = [0; 16];
    for &sig in signals {
        let bit = u32::try_from(sig - 1).expect("a signal number");
        #[cfg(target_vendor = "apple")]
        {
            set |= 1 << bit;
        }
        #[cfg(not(target_vendor = "apple"))]
        {
            set[usize::try_from(bit / 64).expect("a word")] |= 1 << (bit % 64);
        }
    }
    set
}

/// The signals this thread blocks, comma-separated.
fn blocked() -> String {
    let mut mask = set_of(&[]);
    // SAFETY: a null `set` only reads this thread's mask into a live local.
    assert_eq!(
        unsafe { pthread_sigmask(SIG_BLOCK, std::ptr::null(), &mut mask) },
        0
    );
    (1..=if cfg!(target_vendor = "apple") {
        32
    } else {
        64
    })
        .filter(|&sig| has(&mask, sig))
        .map(|sig| sig.to_string())
        .collect::<Vec<_>>()
        .join(",")
}

/// Runs `/bin/sh -c <script> sh <out>` via `std` or [`transparent`] and returns `(status, output)`.
fn sh_run(transparent_run: bool, script: &str, out: &std::path::Path) -> (ExitStatus, String) {
    let _ = std::fs::remove_file(out);
    let args = [
        "-c",
        script,
        "sh",
        out.to_str().expect("a UTF-8 scratch path"),
    ];
    let status = if transparent_run {
        transparent("/bin/sh").args(args).status()
    } else {
        std::process::Command::new("/bin/sh").args(args).status()
    }
    .expect("run /bin/sh");
    (status, std::fs::read_to_string(out).unwrap_or_default())
}

/// A variable set on a [`transparent`] command reaches its child over this process's own, the
/// last set of a key winning, and the rest of the environment is inherited. NEGATIVE CONTROL: the
/// same command with nothing set hands the child this process's `HOME` and no such variable.
#[test]
fn a_variable_set_on_a_transparent_command_reaches_its_child_over_the_inherited_ones() {
    let dir = std::env::temp_dir().join(format!("spawnfd-transparent-env-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let out = dir.join("out");
    let out_s = out.to_str().expect("a UTF-8 scratch path");
    let script = "printf '%s|%s|%s' \"${ATERM_UDS_SET-unset}\" \"$HOME\" \"${ATERM_UDS_TWICE-unset}\" > \"$1\"";
    let home = std::env::var("HOME").unwrap_or_default();
    let status = transparent("/bin/sh")
        .args(["-c", script, "sh", out_s])
        .env("ATERM_UDS_SET", "on")
        .env("HOME", "/elsewhere")
        .env("ATERM_UDS_TWICE", "first")
        .env("ATERM_UDS_TWICE", "second")
        .status()
        .expect("run /bin/sh");
    assert!(status.success());
    assert_eq!(
        std::fs::read_to_string(&out).expect("its output"),
        "on|/elsewhere|second"
    );
    let status = transparent("/bin/sh")
        .args(["-c", script, "sh", out_s])
        .status()
        .expect("run /bin/sh");
    assert!(status.success());
    assert_eq!(
        std::fs::read_to_string(&out).expect("its output"),
        format!("unset|{home}|unset")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The staged half of [`a_transparent_run_is_the_std_run_it_replaces`].
fn staged_signals() {
    // SAFETY: `signal` with `SIG_IGN` or `SIG_DFL`, and a mask change for this
    // thread; each takes plain integers or a live local.
    unsafe {
        signal(SIGHUP, SIG_DFL);
        signal(SIGUSR2, SIG_IGN);
        let block = set_of(&[SIGUSR1, SIGTERM]);
        assert_eq!(pthread_sigmask(SIG_BLOCK, &block, std::ptr::null_mut()), 0);
    }
    let dir = std::env::temp_dir().join(format!("spawnfd-transparent-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let out: PathBuf = dir.join("out");
    let exe = std::env::current_exe().expect("the test binary");
    let exe = exe.to_str().expect("a UTF-8 test binary path");
    let cases = [
        ("exit code", "exit 7".to_string()),
        ("death by signal", "kill -HUP $$".to_string()),
        (
            "stdio: the same three open files",
            format!(
                "exec 7<&0 8>&1 9>&2; {STAGE_ENV}={STDIO_PROBE_STAGE} exec '{exe}' \
                 --exact spawnfd::transparent_tests::a_transparent_run_is_the_std_run_it_replaces \
                 --nocapture --test-threads=1 >\"$1\""
            ),
        ),
        (
            "dispositions: SIGPIPE at its default, SIGUSR2 still ignored",
            "(yes 2>/dev/null; echo \"yes:$?\" >>\"$1\") | head -c1 >/dev/null; \
             kill -USR2 $$; echo survived >>\"$1\""
                .to_string(),
        ),
        (
            "the signal mask: the spawning thread's",
            format!(
                "{STAGE_ENV}={MASK_PROBE_STAGE} exec '{exe}' \
                 --exact spawnfd::transparent_tests::a_transparent_run_is_the_std_run_it_replaces \
                 --nocapture --test-threads=1 >\"$1\""
            ),
        ),
    ];
    let read = |what: &str, wrote: String| {
        let tag = if what.starts_with("the signal mask") {
            "MASK="
        } else if what.starts_with("stdio") {
            "STDIO="
        } else {
            return wrote;
        };
        wrote
            .lines()
            .find(|line| line.starts_with(tag))
            .unwrap_or_else(|| panic!("the probe printed no {tag} line: {wrote:?}"))
            .to_string()
    };
    for (what, script) in &cases {
        let theirs = sh_run(false, script, &out);
        let theirs = (theirs.0, read(what, theirs.1));
        let ours = sh_run(true, script, &out);
        let ours = (ours.0, read(what, ours.1));
        println!("STAGED {what}: ours={ours:?} std={theirs:?}");
        assert_eq!(ours.0.into_raw(), theirs.0.into_raw(), "{what}: the status");
        assert_eq!(ours.1, theirs.1, "{what}: what it wrote");
    }
    let (status, _) = sh_run(true, &cases[0].1, &out);
    assert_eq!(status.code(), Some(7));
    let (status, _) = sh_run(true, &cases[1].1, &out);
    assert_eq!(status.signal(), Some(SIGHUP));
    let (_, stdio) = sh_run(true, &cases[2].1, &out);
    let stdio = read(cases[2].0, stdio);
    assert!(
        stdio.starts_with("STDIO=") && stdio.matches(',').count() == 2,
        "{stdio:?}"
    );
    let (_, dispositions) = sh_run(true, &cases[3].1, &out);
    assert_eq!(dispositions, "yes:141\nsurvived\n");
    let (_, mask) = sh_run(true, &cases[4].1, &out);
    let mut want = [SIGTERM, SIGUSR1];
    want.sort_unstable();
    let want = format!("MASK={},{}", want[0], want[1]);
    assert!(mask.lines().any(|line| line == want), "{want} in {mask:?}");
    let _ = std::fs::remove_dir_all(&dir);
    println!("STAGED transparent signals ok");
}

/// [`transparent`] matches `std::process::Command` across exit status, signal death, stdio descriptors, signal dispositions, and thread signal masks.
#[test]
fn a_transparent_run_is_the_std_run_it_replaces() {
    const NAME: &str = "spawnfd::transparent_tests::a_transparent_run_is_the_std_run_it_replaces";
    if transparent_mask_probe_stage_if_asked() {
        return;
    }
    if transparent_stdio_probe_stage_if_asked() {
        return;
    }
    if std::env::var(STAGE_ENV).is_ok() {
        staged_signals();
        return;
    }
    let out = run_staged_hup_ignored(NAME, SIGNALS_STAGE);
    assert!(out.contains("STAGED transparent signals ok"), "{out}");
    print!("{out}");
}

#[test]
fn a_refused_transparent_exec_preserves_the_original_mask_actions_and_descriptors() {
    const NAME: &str = "spawnfd::transparent_tests::a_refused_transparent_exec_preserves_the_original_mask_actions_and_descriptors";
    if std::env::var(STAGE_ENV).as_deref() != Ok("transparent-exec-refused") {
        let out = run_staged_hup_ignored(NAME, "transparent-exec-refused");
        assert!(out.contains("STAGED transparent exec refused ok"), "{out}");
        return;
    }
    let dir = aterm_tempfile::Builder::new()
        .prefix("transparent-exec-refused")
        .tempdir()
        .unwrap();
    let path = dir.path().join("not-executable");
    std::fs::write(&path, b"not executable fixture data").unwrap();
    let (_near, far) = std::os::unix::net::UnixStream::pair().unwrap();
    let fd = far.as_raw_fd();
    let before_flags = super::tests::flags(fd);
    // SAFETY: this is an isolated subprocess; the two plain dispositions and the current
    // thread's mask are intentionally changed before its attempted exec, never in the suite.
    unsafe {
        signal(libc::SIGPIPE, SIG_IGN);
        signal(SIGUSR2, SIG_IGN);
        let block = set_of(&[SIGUSR1]);
        assert_eq!(pthread_sigmask(SIG_BLOCK, &block, std::ptr::null_mut()), 0);
    }
    let action = || {
        // SAFETY: the query writes one fully initialized action into this live slot.
        let mut action = std::mem::MaybeUninit::<libc::sigaction>::uninit();
        assert_eq!(
            unsafe { libc::sigaction(libc::SIGPIPE, std::ptr::null(), action.as_mut_ptr()) },
            0
        );
        // SAFETY: the successful query initialized the action.
        unsafe { action.assume_init() }
    };
    let before = action();
    let refused = transparent(&path).exec();
    assert_eq!(refused.raw_os_error(), Some(libc::EACCES));
    // The prepared path reaches the same real native refusal, then restores the same state.
    let refused = transparent(&path)
        .prepare_exec()
        .expect("prepare the native data-file refusal")
        .exec_until(u64::MAX);
    assert_eq!(refused.raw_os_error(), Some(libc::EACCES));
    let refused = transparent(&path)
        .prepare_exec()
        .expect("prepare without dispatch")
        .exec_until(0);
    assert_eq!(refused.kind(), io::ErrorKind::TimedOut);
    let after = action();
    assert_eq!(before.sa_sigaction, after.sa_sigaction);
    assert_eq!(before.sa_flags, after.sa_flags);
    assert_eq!(super::tests::flags(fd), before_flags);
    let mut blocked = set_of(&[]);
    // SAFETY: the existing platform-sized set is writable; a null input only reads the mask.
    assert_eq!(
        unsafe { pthread_sigmask(SIG_BLOCK, std::ptr::null(), &mut blocked) },
        0
    );
    assert!(has(&blocked, SIGUSR1));
    #[cfg(target_os = "linux")]
    {
        // Actual negative control: the ordinary stripping exec is deliberately unsuitable
        // for a rollback-preserving failure. Its std/pre_exec path clears this mask/action.
        let refused = command(&path).exec();
        assert_eq!(refused.raw_os_error(), Some(libc::EACCES));
        assert_ne!(action().sa_sigaction, before.sa_sigaction);
        // SAFETY: the same read-only query into the live platform-sized set.
        assert_eq!(
            unsafe { pthread_sigmask(SIG_BLOCK, std::ptr::null(), &mut blocked) },
            0
        );
        assert!(!has(&blocked, SIGUSR1));
    }
    println!("STAGED transparent exec refused ok");
}

/// Exercise the prepared platform dispatch path with a deterministic clock and a recording
/// native-call substitute. Signal changes happen only in the existing isolated test child.
#[cfg(any(target_vendor = "apple", target_os = "linux"))]
#[test]
fn a_prepared_exec_captures_before_go_and_checks_the_original_deadline_at_dispatch() {
    const NAME: &str = "spawnfd::transparent_tests::a_prepared_exec_captures_before_go_and_checks_the_original_deadline_at_dispatch";
    if std::env::var(STAGE_ENV).as_deref() != Ok("transparent-exec-prepared") {
        let out = run_staged_hup_ignored(NAME, "transparent-exec-prepared");
        assert!(out.contains("STAGED transparent exec prepared ok"), "{out}");
        return;
    }
    use std::cell::Cell;
    use std::os::unix::ffi::OsStrExt as _;
    let flags = std::fs::File::open("/dev/null").unwrap();
    let before_flags = super::tests::flags(flags.as_raw_fd());
    // SAFETY: this is the isolated test child; no suite thread shares these dispositions.
    unsafe {
        signal(libc::SIGPIPE, SIG_IGN);
        let mask = set_of(&[SIGUSR1]);
        assert_eq!(pthread_sigmask(SIG_BLOCK, &mask, std::ptr::null_mut()), 0);
    }
    let before_mask = blocked();
    let sigpipe = || {
        let mut action = std::mem::MaybeUninit::<libc::sigaction>::uninit();
        // SAFETY: a read-only query initializes the writable, correctly sized action slot.
        assert_eq!(
            unsafe { libc::sigaction(libc::SIGPIPE, std::ptr::null(), action.as_mut_ptr()) },
            0
        );
        // SAFETY: the successful query initialized this action.
        unsafe { action.assume_init() }.sa_sigaction
    };
    let before_action = sigpipe();
    let mut command = transparent("/nonexistent/prepared-native-image");
    command
        .arg0("original-agent-name")
        .arg("captured")
        .env("ATERM_PREPARED_ENV", "before-go");
    let prepared = command.prepare_exec().expect("prepare exact path");
    command
        .arg0("too-late-name")
        .arg("too-late")
        .env("ATERM_PREPARED_ENV", "after-go");
    let (image, path) = prepared.captured();
    assert_eq!(path.to_bytes(), b"/nonexistent/prepared-native-image");
    assert_eq!(image.argv.len(), 2);
    assert_eq!(image.argv[0].to_bytes(), b"original-agent-name");
    assert_eq!(image.argv[1].to_bytes(), b"captured");
    assert!(
        image
            .envp
            .iter()
            .any(|word| word.to_bytes() == b"ATERM_PREPARED_ENV=before-go")
    );
    assert!(
        !image
            .envp
            .iter()
            .any(|word| word.to_bytes() == b"ATERM_PREPARED_ENV=after-go")
    );

    let dispatched = Cell::new(0);
    // A pre-Go observation was timely; the actual platform path must reject the now-expired
    // original deadline rather than accepting that stale observation or starting a new budget.
    let observed = Cell::new(99);
    assert!(observed.get() < 100);
    observed.set(100);
    let refused = prepared.exec_with(
        100,
        || Ok(observed.get()),
        || {
            dispatched.set(dispatched.get() + 1);
            io::Error::from_raw_os_error(libc::EACCES)
        },
    );
    assert_eq!(refused.kind(), io::ErrorKind::TimedOut);
    assert_eq!(dispatched.get(), 0);
    for (clock, expected, calls) in [
        (99, io::ErrorKind::PermissionDenied, 1),
        (101, io::ErrorKind::TimedOut, 0),
    ] {
        let before = dispatched.get();
        let prepared = command
            .prepare_exec()
            .expect("fresh prepared dispatch control");
        let refused = prepared.exec_with(
            100,
            || Ok(clock),
            || {
                dispatched.set(dispatched.get() + 1);
                io::Error::from_raw_os_error(libc::EACCES)
            },
        );
        assert_eq!(refused.kind(), expected);
        assert_eq!(dispatched.get() - before, calls);
        assert_eq!(sigpipe(), before_action);
        assert_eq!(blocked(), before_mask);
        assert_eq!(super::tests::flags(flags.as_raw_fd()), before_flags);
    }
    let refused = command.prepare_exec().unwrap().exec_with(
        100,
        || Err(io::Error::from_raw_os_error(libc::EIO)),
        || {
            dispatched.set(dispatched.get() + 1);
            io::Error::from_raw_os_error(libc::EACCES)
        },
    );
    assert_eq!(refused.raw_os_error(), Some(libc::EIO));
    assert_eq!(dispatched.get(), 1);
    assert_eq!(sigpipe(), before_action);
    assert_eq!(blocked(), before_mask);
    assert_eq!(super::tests::flags(flags.as_raw_fd()), before_flags);
    let malformed = transparent(std::ffi::OsStr::from_bytes(b"bad\0program"))
        .prepare_exec()
        .err()
        .expect("preparation rejects NUL before Go");
    assert_eq!(malformed.kind(), io::ErrorKind::InvalidInput);

    // Exact dispatch-gate ordering control: a gate that consumes the remaining budget must be
    // entered before the clock is read. The native callback must not run after that wait.
    let at = Cell::new(99);
    let refused = exec_dispatch(
        || at.set(100),
        100,
        || Ok(at.get()),
        || {
            dispatched.set(dispatched.get() + 1);
            io::Error::from_raw_os_error(libc::EACCES)
        },
    );
    assert_eq!(refused.kind(), io::ErrorKind::TimedOut);
    assert_eq!(dispatched.get(), 1);
    println!("STAGED transparent exec prepared ok");
}
