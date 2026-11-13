// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// A moldable test's threads and CPU charge are different quantities. The child must run
/// the planned width while nested builds retain the admitted CPU ceiling, and each of its test
/// threads is granted its share of the binary's charge (seven cores over three threads: two
/// each) for its own pools, in place of a stale grant its record carried. NEGATIVE CONTROL:
/// an ordinary command with the same environment still receives the core-bound thread cap,
/// and no grant.
#[cfg(unix)]
#[test]
fn a_planned_test_width_survives_its_smaller_core_charge() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let ledger = aterm_capacity::Ledger::at(tmp.path().join("ledger"));
    let machine = aterm_capacity::Fixed::new(aterm_capacity::Reading::of_gib(48, 40, 14));
    let claim = aterm_capacity::admit_with(
        &ledger,
        &machine,
        aterm_capacity::Need::new("one-core test charge")
            .mem(64 << 20)
            .cores(1)
            .covers(),
        aterm_capacity::Admit::Wait,
        aterm_capacity::Pace::default(),
        &mut |_| {},
    )
    .unwrap();
    assert_eq!(claim.cores(), 1);
    let _resources = crate::holding_resource_claim(Some(std::sync::Arc::new(claim)));
    let binary = crate::testrun::Binary {
        harness: false,
        header: String::new(),
        spec: "planned-width fixture".into(),
        record: crate::testrun::Record {
            cwd: tmp.path().to_path_buf(),
            argv: shell_argv(
                "printf '%s %s %s %s' \"$CARGO_BUILD_JOBS\" \"$BUILD_SLOT_HELD\" \
                 \"$RUST_TEST_THREADS\" \"${ATERM_TEST_CORES-unset}\"",
                "",
            ),
            env: vec![
                ("CARGO_BUILD_JOBS".into(), "9".into()),
                ("RUST_TEST_THREADS".into(), "9".into()),
            ],
        },
    };
    let planned = crate::testrun::binary_cmd(
        &binary,
        3,
        7,
        false,
        crate::testrun::Watch {
            json: false,
            stop_at_red: false,
            seed: None,
        },
    );
    let ordinary =
        Cmd::replayed(&binary.record.argv, tmp.path(), binary.record.env.clone()).counting_waits();
    let mut stale = binary.clone();
    stale
        .record
        .env
        .push((aterm_cores::GRANT_ENV.into(), "16".into()));
    let restated =
        crate::testrun::binary_cmd(&stale, 3, 7, false, crate::testrun::Watch::default());
    for finalized in [false, true] {
        for (command, expected) in [
            (&ordinary, "1 1 1 unset"),
            (&planned, "1 1 3 2"),
            (&restated, "1 1 3 2"),
        ] {
            let command = if finalized {
                finalize_exact_command(command.clone(), env_in(tmp.path())).unwrap()
            } else {
                command.clone()
            };
            let result = run(&command, env_in(tmp.path()));
            assert!(result.ok, "{}", result.output);
            assert_eq!(said(&result).trim(), expected, "finalized={finalized}");
        }
    }
    let finalized = finalize_exact_command(planned, env_in(tmp.path())).unwrap();
    assert!(finalized.runtime_environment_is_finalized());
    let mut changed = finalized.clone();
    changed.planned_test_threads = Some(2);
    assert!(
        !changed.runtime_environment_is_finalized(),
        "the receipt binds the planned width"
    );
    assert!(
        !finalized
            .test_width(2, 7)
            .runtime_environment_is_finalized(),
        "a new width thaws the receipt"
    );
}

/// Each test thread's grant is its equal share of the binary's charge, rounded down so the
/// threads together stay within it, and at least one. NEGATIVE CONTROL: a charge of one core
/// per thread grants exactly one, the grant an unseen binary (charged its width) gets.
#[test]
fn each_test_thread_is_granted_its_share_of_the_charge() {
    assert_eq!(test_thread_cores(3, 7), 2, "seven cores over three threads");
    assert_eq!(
        test_thread_cores(1, 17),
        17,
        "one thread holds the whole charge"
    );
    assert_eq!(
        test_thread_cores(4, 2),
        1,
        "fewer cores than threads: one each"
    );
    assert_eq!(test_thread_cores(0, 5), 5, "a width of zero is one thread");
    assert_eq!(
        test_thread_cores(4, 0),
        1,
        "no charge still runs a pool of one"
    );
    assert_eq!(test_thread_cores(4, 4), 1, "CONTROL: one core per thread");
}

/// A VARIABLE SET TWICE IS SET ONCE, to its last value, where it was first set: what the child
/// sees, named once. NEGATIVE CONTROL: two variables keep both, in the order they were set.
#[test]
fn a_variable_set_twice_is_named_once_with_its_last_value() {
    let cmd = Cmd::new("targo")
        .env("ATERM_SPEC_CHECKERS", "1")
        .env("TRAIL_LAWS_FULL", "1")
        .env("ATERM_SPEC_CHECKERS", "0");
    assert_eq!(
        cmd.envs,
        [
            ("ATERM_SPEC_CHECKERS".into(), "0".into()),
            ("TRAIL_LAWS_FULL".into(), "1".into())
        ]
    );
}

/// THE MACHINE'S CORES time a child's tree: the live host's count is `aterm_capacity::cores`,
/// sysprobe's logical cores, not this process's parallelism (`Ctx::parallelism`), which an
/// affinity mask or a cgroup quota narrows. The layout reads on macOS and Linux, so the count
/// is the machine's and not the floor of one that stands in for an unread layout. NEGATIVE
/// CONTROL, on Linux: a thread narrowed to one core of a machine of more reads a parallelism of
/// one and the live host's count unchanged, so a count read from the parallelism fails. macOS
/// has no affinity that narrows it, so there the two counts agree and only the first half
/// discriminates.
#[test]
fn the_live_hosts_cores_are_the_machines_logical_cores() {
    let logical = aterm_sysprobe::cpu_layout().logical_cores();
    let machine = u32::try_from(logical).unwrap_or(u32::MAX).max(1);
    assert_eq!(LiveHost.cores(), machine);
    if cfg!(any(target_os = "macos", target_os = "linux")) {
        assert!(logical > 0, "the layout reads on this host");
    }
    #[cfg(target_os = "linux")]
    if logical > 1 {
        let (parallelism, cores) = std::thread::spawn(|| {
            unsafe extern "C" {
                /// `<sched.h>`; aterm-libc does not export it. `mask` is a `cpu_set_t`.
                fn sched_getaffinity(pid: i32, size: usize, mask: *mut [u64; 16]) -> i32;
                /// `<sched.h>`.
                fn sched_setaffinity(pid: i32, size: usize, mask: *const [u64; 16]) -> i32;
            }
            let mut allowed = [0u64; 16];
            // SAFETY: `allowed` is a writable 1024-bit `cpu_set_t` of the size passed; pid 0
            // is this thread.
            let rc =
                unsafe { sched_getaffinity(0, std::mem::size_of_val(&allowed), &raw mut allowed) };
            assert_eq!(rc, 0, "{}", std::io::Error::last_os_error());
            let word = allowed
                .iter()
                .position(|w| *w != 0)
                .expect("an allowed core");
            let mut one = [0u64; 16];
            one[word] = 1 << allowed[word].trailing_zeros();
            // SAFETY: `one` is a readable `cpu_set_t` of the size passed, naming one core
            // this thread may run on; pid 0 narrows this thread alone.
            let rc = unsafe { sched_setaffinity(0, std::mem::size_of_val(&one), &raw const one) };
            assert_eq!(rc, 0, "{}", std::io::Error::last_os_error());
            (
                aterm_cores::available_parallelism().map_or(0, std::num::NonZeroUsize::get),
                LiveHost.cores(),
            )
        })
        .join()
        .expect("the narrowed thread");
        assert_eq!(parallelism, 1, "the thread is narrowed to one core");
        assert_eq!(
            cores, machine,
            "the live host still reads the machine's cores"
        );
    }
}

/// A command-local removal wins over the run's inherited additions, while an explicit value
/// on that same command wins over its removal, matching `Command`. NEGATIVE CONTROL: an
/// unrelated variable remains present.
#[test]
fn a_command_can_remove_one_inherited_variable_without_clearing_the_rest() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let removed = run(
        &Cmd::new("/usr/bin/env").remove_env("PATH"),
        env_in(dir.path()),
    );
    assert!(removed.ok, "{}", removed.output);
    assert!(!removed.output.lines().any(|line| line.starts_with("PATH=")));
    assert!(
        removed.output.lines().any(|line| line.starts_with("HOME=")),
        "an unrelated inherited variable remains"
    );
    let restored = run(
        &Cmd::new("/usr/bin/env")
            .remove_env("PATH")
            .env("PATH", "/restored"),
        env_in(dir.path()),
    );
    assert!(restored.output.lines().any(|line| line == "PATH=/restored"));
}

#[cfg(unix)]
#[test]
fn a_native_stage_keeps_its_inherited_pipe_descriptor() {
    use std::io::Write as _;
    use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
    let dir = aterm_tempfile::tempdir().unwrap();
    let (reader, mut writer) = aterm_uds::pipe().unwrap();
    // SAFETY: F_DUPFD creates one owned, non-close-on-exec duplicate without replacing
    // another descriptor. The original remains live throughout the call.
    let fd = unsafe { libc::fcntl(reader.as_raw_fd(), libc::F_DUPFD, 100) };
    assert!(fd >= 100);
    // SAFETY: the successful duplicate is newly owned by this test alone.
    let inherited = unsafe { OwnedFd::from_raw_fd(fd) };
    writer.write_all(b"shared-jobserver-handle\n").unwrap();
    drop(writer);
    let result = run(
        &Cmd::new("/bin/cat").arg(format!("/dev/fd/{fd}")),
        env_in(dir.path()),
    );
    assert!(result.ok, "{}", result.output);
    assert_eq!(result.output, "shared-jobserver-handle\n");
    drop(inherited);
}

fn env_in(dir: &Path) -> ExecEnv<'_> {
    ceiled(dir, Some(DEFAULT_CHILD_CEILING))
}

#[cfg(unix)]
#[test]
fn a_moldable_compile_passes_its_admitted_width_to_cargo_and_restores_it() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let cmd = Cmd::new("/usr/bin/printenv")
        .arg("CARGO_BUILD_JOBS")
        .env("CARGO_BUILD_JOBS", "16");
    let narrowed = crate::compile_width::with_jobs(Some(2), || run(&cmd, env_in(dir.path())));
    assert!(narrowed.ok, "{}", narrowed.output);
    assert_eq!(narrowed.output.trim(), "2");
    let restored = run(&cmd, env_in(dir.path()));
    assert!(restored.ok, "{}", restored.output);
    assert_eq!(restored.output.trim(), "16");
}

#[cfg(unix)]
#[test]
fn compiler_admission_refusal_is_environmental_but_a_compiler_error_is_not() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut failed = run(&Cmd::new("/usr/bin/false"), env_in(dir.path()));
    failed.output = "aterm-verify: compiler admission: unit memory history disappeared\nerror: could not compile\n".into();
    assert_eq!(
        crate::compile_width::refusal(&failed, "").as_deref(),
        Some("unit memory history disappeared")
    );
    // The marker is meaningful only to the compile stage, never to a failed test.
    assert_eq!(failed.environment_failure(), None);
    assert_eq!(
        crate::compile_width::refusal(
            &failed,
            r#"{"reason":"compiler-message","message":{"level":"error"}}"#
        ),
        None
    );
    failed.output = "error: unresolved import\n".into();
    assert_eq!(crate::compile_width::refusal(&failed, ""), None);
    failed.output = "source: aterm-verify: compiler admission: quoted source text\n".into();
    assert_eq!(crate::compile_width::refusal(&failed, ""), None);
}

fn ceiled(dir: &Path, child_ceiling: Option<Duration>) -> ExecEnv<'_> {
    ExecEnv {
        events: None,
        cwd: dir,
        path: OsStr::new("/usr/bin:/bin"),
        scratch: dir,
        child_ceiling,
        remove_env: &[],
        add_env: &[],
        stage_groups: None,
        host: &INERT,
        hang_samples: None,
    }
}

/// A host that reads nothing, or only the input clock it is given: every span with no marked
/// wait is work, as before the clock read its tree.
#[derive(Debug)]
pub(super) struct Idle(pub(super) Option<Duration>);

impl Host for Idle {
    fn group_usage(&self, _: u32) -> Option<Vec<aterm_sysprobe::MemberUsage>> {
        None
    }

    fn idle(&self) -> Option<Duration> {
        self.0
    }

    fn cores(&self) -> u32 {
        1
    }
}

/// No reading of anything.
pub(super) static INERT: Idle = Idle(None);

/// A stand-in host for a tree that is runnable all the time and is given `share` per mille of
/// that time on a core: the child's group as one member, whose counters run with the wall
/// clock between reads, or, booked in `lumps`, stay flat until that long has passed since they
/// last grew and then grow by all of it, as macOS books a starved thread's time when it next
/// gets a core.
#[cfg(unix)]
#[derive(Debug)]
struct Starving {
    share: AtomicU64,
    lumps: Option<Duration>,
    counters: std::sync::Mutex<Option<(Instant, u64, u64)>>,
}

#[cfg(unix)]
impl Starving {
    fn at(per_mille: u64) -> &'static Self {
        Self::booked(per_mille, None)
    }

    fn lumpy(per_mille: u64, every: Duration) -> &'static Self {
        Self::booked(per_mille, Some(every))
    }

    fn booked(per_mille: u64, lumps: Option<Duration>) -> &'static Self {
        Box::leak(Box::new(Self {
            share: AtomicU64::new(per_mille),
            lumps,
            counters: std::sync::Mutex::new(None),
        }))
    }

    fn give(&self, per_mille: u64) {
        self.share.store(per_mille, Ordering::SeqCst);
    }
}

#[cfg(unix)]
impl Host for Starving {
    fn group_usage(&self, pgid: u32) -> Option<Vec<aterm_sysprobe::MemberUsage>> {
        let mut c = self
            .counters
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let now = Instant::now();
        let (at, cpu, runnable) = c.get_or_insert((now, 0, 0));
        let since = now.saturating_duration_since(*at);
        if self.lumps.is_none_or(|every| since >= every) {
            let span = u64::try_from(since.as_nanos()).unwrap_or(u64::MAX);
            *at = now;
            *runnable += span;
            *cpu += span / 1000 * self.share.load(Ordering::SeqCst);
        }
        Some(vec![aterm_sysprobe::MemberUsage {
            pid: pgid,
            start: 1,
            cpu_ns: *cpu,
            runnable_ns: *runnable,
            io_bytes: 0,
            ..aterm_sysprobe::MemberUsage::default()
        }])
    }

    fn idle(&self) -> Option<Duration> {
        None
    }

    fn cores(&self) -> u32 {
        1
    }
}

#[cfg(unix)]
#[test]
fn a_starred_removal_takes_every_inherited_variable_it_begins() {
    let tmp = crate::mktemp_dir("atv-env-prefix").expect("mktemp");
    let env_of = |remove: &[&str]| {
        let env = ExecEnv {
            remove_env: remove,
            ..ceiled(&tmp, Some(Duration::from_secs(60)))
        };
        run(&Cmd::new("/usr/bin/env"), env).output
    };
    let removed = env_of(&["CARGO_PKG_*", "CARGO_MANIFEST_DIR"]);
    assert!(
        !removed
            .lines()
            .any(|l| l.starts_with("CARGO_PKG_") || l.starts_with("CARGO_MANIFEST_DIR=")),
        "{removed}"
    );
    if std::env::var_os("CARGO_PKG_NAME").is_some() {
        let kept = env_of(&["CARGO_PKG"]);
        assert!(
            kept.lines().any(|l| l.starts_with("CARGO_PKG_NAME=")),
            "{kept}"
        );
    }
    std::fs::remove_dir_all(&tmp).ok();
}

/// The host shell's argv for a script: `unix` under `/bin/sh -c`, or off Unix `windows` under
/// `cmd.exe /d /s /c`, which strips the quotes std puts round an argument with a space and
/// leaves a variable it does not have as its literal `%NAME%`.
fn shell_argv(unix: &str, windows: &str) -> Vec<OsString> {
    if cfg!(unix) {
        vec!["/bin/sh".into(), "-c".into(), unix.into()]
    } else {
        let cmd = std::env::var_os("ComSpec")
            .unwrap_or_else(|| OsString::from(r"C:\Windows\System32\cmd.exe"));
        vec![cmd, "/d".into(), "/s".into(), "/c".into(), windows.into()]
    }
}

fn shell(unix: &str, windows: &str) -> Cmd {
    let argv = shell_argv(unix, windows);
    Cmd::new(&argv[0]).args(&argv[1..])
}

/// What a child said, its line ends LF as `/bin/sh`'s are (cmd.exe ends each with CRLF).
fn said(r: &Run) -> String {
    let text = if cfg!(windows) {
        r.output.replace("\r\n", "\n")
    } else {
        r.output.clone()
    };
    text.trim_end_matches('\n').to_owned()
}

/// What the host shell's script reads for a variable the child does not have: the script's
/// `${NAME-unset}`, or cmd.exe's untouched `%NAME%`.
fn unset(name: &str) -> String {
    if cfg!(unix) {
        "unset".to_owned()
    } else {
        format!("%{name}%")
    }
}

/// A variable every test run inherits, which a child that keeps only its record must not.
const INHERITED: &str = if cfg!(unix) { "HOME" } else { "USERPROFILE" };

/// Only the isolated fixture reads its inherited dispositions; ordinary suite runs do not
/// assert anything about the launcher that started libtest.
#[cfg(unix)]
#[test]
fn signal_disposition_probe() {
    let Ok(expected) = std::env::var("ATERM_VERIFY_SIGNAL_PROBE") else {
        return;
    };
    let expected = if expected == "ignored" {
        libc::SIG_IGN
    } else {
        libc::SIG_DFL
    };
    for sig in [
        libc::SIGINT,
        libc::SIGQUIT,
        libc::SIGTERM,
        libc::SIGHUP,
        libc::SIGUSR1,
        libc::SIGUSR2,
        libc::SIGALRM,
    ] {
        let mut action = std::mem::MaybeUninit::<libc::sigaction>::zeroed();
        // SAFETY: a null new action only reads the disposition into the live local.
        assert_eq!(
            unsafe { libc::sigaction(sig, std::ptr::null(), action.as_mut_ptr()) },
            0
        );
        // SAFETY: the successful query initialized the action.
        assert_eq!(
            unsafe { action.assume_init() }.sa_sigaction,
            expected,
            "signal {sig}"
        );
    }
    println!("\nSIGNALS_CHECKED");
}

/// Verify's actual test runner and direct measurement commands discard a parent's ignores.
/// The ordinary spawn is the negative control: it must observe SIG_IGN, not SIG_DFL.
#[cfg(unix)]
#[test]
fn test_runner_defaults_signals_from_an_ignoring_parent() {
    const PARENT: &str = "ATERM_VERIFY_SIGNAL_PARENT";
    const NAME: &str = "exec::tests::test_runner_defaults_signals_from_an_ignoring_parent";
    const PROBE: &str = "exec::tests::signal_disposition_probe";
    let me = std::env::current_exe().unwrap();
    if std::env::var_os(PARENT).is_none() {
        let mut parent = aterm_uds::spawnfd::command("/bin/sh");
        parent
            .args([
                "-c",
                "trap '' INT QUIT TERM HUP USR1 USR2 ALRM PIPE; exec \"$0\" \"$@\"",
            ])
            .arg(&me)
            .args(["--exact", NAME, "--nocapture", "--test-threads=1"])
            .env(PARENT, "1");
        let out = aterm_uds::deadline::ends_within(&mut parent, Duration::from_secs(60));
        assert!(out.status.success(), "{out:?}");
        assert!(
            String::from_utf8_lossy(&out.stdout).contains("\nSIGNAL_RUNNER_CHECKED\n"),
            "{out:?}"
        );
        return;
    }
    let args = ["--exact", PROBE, "--nocapture", "--test-threads=1"];
    let mut control = aterm_uds::spawnfd::command(&me);
    control
        .args(args)
        .env("ATERM_VERIFY_SIGNAL_PROBE", "ignored");
    let out = aterm_uds::deadline::ends_within(&mut control, Duration::from_secs(60));
    assert!(out.status.success(), "inherited-ignore control: {out:?}");
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("\nSIGNALS_CHECKED\n"),
        "{out:?}"
    );

    let dir = aterm_tempfile::tempdir().unwrap();
    let cmd = Cmd::new(&me)
        .args(args)
        .env("ATERM_VERIFY_SIGNAL_PROBE", "default");
    let ran = run_test(&cmd, env_in(dir.path()), &|| {});
    assert!(ran.ok, "test runner: {}", ran.output);
    assert!(ran.output.contains("\nSIGNALS_CHECKED\n"), "{}", ran.output);

    let mut measured = check_command(&me);
    measured
        .args(args)
        .env("ATERM_VERIFY_SIGNAL_PROBE", "default");
    let out = aterm_uds::deadline::ends_within(&mut measured, Duration::from_secs(60));
    assert!(out.status.success(), "measurement runner: {out:?}");
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("\nSIGNALS_CHECKED\n"),
        "{out:?}"
    );
    println!("\nSIGNAL_RUNNER_CHECKED");
}

#[test]
fn a_child_runs_in_the_root_with_the_runs_variables_and_its_own_win() {
    let tmp = crate::mktemp_dir("atv-setenv").expect("mktemp");
    std::fs::write(tmp.join("marker"), b"x").expect("write");
    let added = [(OsString::from("ATV_ADDED_PROBE"), OsString::from("0"))];
    let env = ExecEnv {
        add_env: &added,
        ..env_in(&tmp)
    };
    let ask = shell(
        "cat marker; printf ' %s %s' \"${ATV_ADDED_PROBE-unset}\" \"$PATH\"",
        "type marker& echo  %ATV_ADDED_PROBE% %PATH%",
    );
    assert_eq!(said(&run(&ask, env)), "x 0 /usr/bin:/bin");
    let stage_says = ask.clone().env("ATV_ADDED_PROBE", "1");
    assert_eq!(said(&run(&stage_says, env)), "x 1 /usr/bin:/bin");
    assert_eq!(
        said(&run(&ask, env_in(&tmp))),
        format!("x {} /usr/bin:/bin", unset("ATV_ADDED_PROBE"))
    );
    std::fs::remove_dir_all(&tmp).ok();
}

/// A child is handed the cover marker of its stage's claim ([`crate::memory::cover`]) over
/// the run's, from the stage's own thread and from a worker of its pool alike. NEGATIVE
/// CONTROL: outside a stage that holds a claim, the run's own (`NO_COVER`) reaches it.
#[test]
fn a_child_is_handed_its_stages_cover_marker() {
    let tmp = crate::mktemp_dir("atv-cover").expect("mktemp");
    let added = [(
        OsString::from(aterm_capacity::COVER_ENV),
        OsString::from(aterm_capacity::NO_COVER),
    )];
    let env = ExecEnv {
        add_env: &added,
        ..env_in(&tmp)
    };
    let ask = shell(
        "printf %s \"$ATERM_CAPACITY_COVER\"",
        "echo %ATERM_CAPACITY_COVER%",
    );
    assert_eq!(said(&run(&ask, env)), aterm_capacity::NO_COVER);
    {
        let _covering = crate::memory::covering(Some("0000000000001-7-0".into()));
        assert_eq!(said(&run(&ask, env)), "0000000000001-7-0");
        let pooled = crate::testrun::pool(1, &[0], 1, |_| said(&run(&ask, env)));
        assert_eq!(pooled, [Some("0000000000001-7-0".to_string())]);
        let dispatched = crate::testrun::dispatch_each(1, |_| said(&run(&ask, env)));
        assert_eq!(dispatched, [Some("0000000000001-7-0".to_string())]);
    }
    assert_eq!(said(&run(&ask, env)), aterm_capacity::NO_COVER);
    std::fs::remove_dir_all(&tmp).ok();
}

/// A child of a combined stage claim receives that claim's actual CPU grant, including
/// through the bounded test workers that retain it. A command's smaller explicit ceiling is
/// preserved. NEGATIVE CONTROL: propagating the requested machine width, omitting the held
/// marker, or losing custody on a worker fails an exact environment assertion.
#[test]
fn a_child_is_bounded_by_its_stages_actual_core_grant() {
    let tmp = crate::mktemp_dir("atv-resource-grant").expect("mktemp");
    let ledger = aterm_capacity::Ledger::at(tmp.join("ledger"));
    let machine = aterm_capacity::Fixed::new(aterm_capacity::Reading::of_gib(48, 40, 14));
    let ordinary = aterm_capacity::admit_with(
        &ledger,
        &machine,
        aterm_capacity::Need::new("ordinary cores").cores(8),
        aterm_capacity::Admit::Wait,
        aterm_capacity::Pace::default(),
        &mut |_| {},
    )
    .expect("ordinary claim");
    let stage = aterm_capacity::admit_with(
        &ledger,
        &machine,
        aterm_capacity::Need::new("background stage")
            .mem_gib(4)
            .cores(14)
            .covers(),
        aterm_capacity::Admit::Wait,
        aterm_capacity::Pace::default(),
        &mut |_| {},
    )
    .expect("stage claim");
    assert_eq!(
        stage.cores(),
        6,
        "the fixture must exercise a partial grant"
    );
    let _resources = crate::holding_resource_claim(Some(std::sync::Arc::new(stage)));
    let ask = shell(
        "printf '%s %s %s' \"$CARGO_BUILD_JOBS\" \"$BUILD_SLOT_HELD\" \"$RUST_TEST_THREADS\"",
        "echo %CARGO_BUILD_JOBS% %BUILD_SLOT_HELD% %RUST_TEST_THREADS%",
    );
    assert_eq!(said(&run(&ask, env_in(&tmp))), "6 1 6");
    assert_eq!(
        said(&run(
            &ask.clone().env("CARGO_BUILD_JOBS", "3"),
            env_in(&tmp)
        )),
        "3 1 6"
    );
    assert_eq!(
        said(&run(
            &ask.clone().env("RUST_TEST_THREADS", "2"),
            env_in(&tmp)
        )),
        "6 1 2"
    );
    let added = [("RUST_TEST_THREADS".into(), "4".into())];
    let mut with_run_ceiling = env_in(&tmp);
    with_run_ceiling.add_env = &added;
    assert_eq!(said(&run(&ask, with_run_ceiling)), "6 1 4");
    assert_eq!(
        crate::testrun::pool(1, &[0], 1, |_| said(&run(&ask, env_in(&tmp)))),
        [Some("6 1 6".to_owned())]
    );
    assert_eq!(
        crate::testrun::dispatch_each(1, |_| said(&run(&ask, env_in(&tmp)))),
        [Some("6 1 6".to_owned())]
    );
    #[cfg(unix)]
    {
        let argv = shell_argv(
            "printf '%s %s %s %s %s' \"$CARGO_BUILD_JOBS\" \"$BUILD_SLOT_HELD\" \
             \"$RUST_TEST_THREADS\" \"$ATERM_CAPACITY_COVER\" \"${ATERM_WAITS-unset}\"",
            "",
        );
        let recorded = Cmd::replayed(
            &argv,
            &tmp,
            vec![
                ("CARGO_BUILD_JOBS".into(), "13".into()),
                ("CARGO_BUILD_JOBS".into(), "12".into()),
                ("RUST_TEST_THREADS".into(), "11".into()),
                ("BUILD_SLOT_HELD".into(), "stale".into()),
                (aterm_capacity::COVER_ENV.into(), "stale".into()),
                (aterm_toolchain::waits::VAR.into(), "/stale/waits".into()),
            ],
        );
        assert!(
            finalize_exact_command(recorded.clone(), env_in(&tmp)).is_err(),
            "a dynamic per-spawn wait directory cannot be content-keyed"
        );
        let _covering = crate::memory::covering(Some("0000000000001-7-0".into()));
        let finalized = finalize_exact_command(recorded.counting_waits(), env_in(&tmp))
            .expect("the counting command has a representable exact environment");
        let value = |name: &str| {
            finalized
                .envs
                .iter()
                .filter(|(held, _)| held == name)
                .map(|(_, value)| value.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(value("CARGO_BUILD_JOBS"), ["6"]);
        assert_eq!(value("RUST_TEST_THREADS"), ["6"]);
        assert_eq!(value("BUILD_SLOT_HELD"), ["1"]);
        assert_eq!(value(aterm_capacity::COVER_ENV), ["0000000000001-7-0"]);
        assert!(value(aterm_toolchain::waits::VAR).is_empty());
        assert_eq!(
            said(&run(&finalized, env_in(&tmp))),
            "6 1 6 0000000000001-7-0 unset",
            "the materialized command and the child receive the same post-claim environment"
        );
    }
    drop(_resources);

    #[cfg(unix)]
    {
        let argv = shell_argv("printf %s \"$CARGO_BUILD_JOBS\"", "");
        let recorded = Cmd::replayed(&argv, &tmp, vec![("CARGO_BUILD_JOBS".into(), "12".into())])
            .counting_waits();
        let finalized = crate::compile_width::with_jobs(Some(4), || {
            finalize_exact_command(recorded, env_in(&tmp))
        })
        .expect("the compiler-width configurator has a representable environment");
        assert!(finalized.runtime_environment_is_finalized());
        assert!(
            !finalized
                .clone()
                .env("CARGO_BUILD_JOBS", "99")
                .runtime_environment_is_finalized(),
            "an environment builder mutation must thaw the receipt"
        );
        for changed in [
            finalized.clone(),
            finalized.clone(),
            finalized.clone(),
            finalized.clone(),
        ]
        .into_iter()
        .enumerate()
        .map(|(index, mut command)| {
            match index {
                0 => command.args.push("changed-argv".into()),
                1 => command.cwd = Some(tmp.join("changed-cwd")),
                2 => command.demoted = true,
                3 => command.envs.push(("RAW_MUTATION".into(), "1".into())),
                _ => unreachable!(),
            }
            command
        }) {
            assert!(
                !changed.runtime_environment_is_finalized(),
                "a public command-field mutation must invalidate the receipt"
            );
        }
        assert_eq!(
            crate::compile_width::with_jobs(Some(2), || { said(&run(&finalized, env_in(&tmp))) }),
            "4",
            "dispatch must execute the finalized environment, not recompute later policy"
        );
    }
    drop(ordinary);
    std::fs::remove_dir_all(&tmp).ok();
}

/// A child replayed from a recorded invocation ([`Cmd::replayed`]) is handed the cover marker
/// of its stage's claim over the one its recorded environment holds, from a worker of the
/// test dispatch too. NEGATIVE CONTROL: outside a stage that holds a claim, its recorded one
/// reaches it.
#[cfg(unix)]
#[test]
fn a_replayed_child_is_handed_its_stages_cover_marker() {
    let tmp = crate::mktemp_dir("atv-replayed-cover").expect("mktemp");
    let env = env_in(&tmp);
    let ask = Cmd::replayed(
        &[
            OsString::from("/bin/sh"),
            OsString::from("-c"),
            OsString::from("printf %s \"$ATERM_CAPACITY_COVER\""),
        ],
        &tmp,
        vec![(
            OsString::from(aterm_capacity::COVER_ENV),
            OsString::from(aterm_capacity::NO_COVER),
        )],
    );
    assert_eq!(said(&run(&ask, env)), aterm_capacity::NO_COVER);
    {
        let _covering = crate::memory::covering(Some("0000000000001-7-0".into()));
        assert_eq!(said(&run(&ask, env)), "0000000000001-7-0");
        let dispatched = crate::testrun::dispatch_each(1, |_| said(&run(&ask, env)));
        assert_eq!(dispatched, [Some("0000000000001-7-0".to_string())]);
    }
    assert_eq!(said(&run(&ask, env)), aterm_capacity::NO_COVER);
    std::fs::remove_dir_all(&tmp).ok();
}

#[cfg(unix)]
#[test]
fn a_child_killed_by_a_signal_says_which_after_what_it_wrote() {
    let tmp = crate::mktemp_dir("atv-signalled").expect("mktemp");
    let run = run(
        &Cmd::new("/bin/sh").args(["-c", "echo started; kill -KILL $$"]),
        env_in(&tmp),
    );
    assert!(!run.ok && run.code.is_none(), "{run:?}");
    assert_eq!(
        run.trimmed_output(),
        "started\naterm-verify: the child was killed by signal 9"
    );
    std::fs::remove_dir_all(&tmp).ok();
}

#[test]
fn a_missing_tool_is_a_failed_run_not_a_panic_and_never_a_pass() {
    let tmp = crate::mktemp_dir("atv-exec3").expect("mktemp");
    let translator = aterm_uds::process::translator();
    let mut mute = Vec::new();
    for (row, cmd) in [
        ("plain", Cmd::new(tmp.join("absent-driver"))),
        ("demoted", Cmd::new(tmp.join("absent-driver")).demoted()),
    ] {
        let r = run(&cmd, env_in(&tmp));
        assert!(!r.ok, "{row}: {r:?}");
        if translator.is_some() && r.spawn_error.is_none() && r.code == Some(127) {
            mute.push(row);
            continue;
        }
        assert!(r.spawn_error.is_some(), "{row}: {r:?}");
    }
    if let Some(translator) = translator
        && !mute.is_empty()
    {
        println!(
            "NOT RUN under {translator}: exec::tests::\
             a_missing_tool_is_a_failed_run_not_a_panic_and_never_a_pass: the spawn error \
             ({}): std's answer comes from glibc's posix_spawn, whose child reports a failed \
             exec through memory it shares with its parent; the translator runs that child in \
             a copy, where the report is lost, so std answers a child that exited 127 (still a \
             failed run); a real CPU of this code's arch decides it",
            mute.join(", ")
        );
    }
    std::fs::remove_dir_all(&tmp).ok();
}

#[test]
fn output_is_captured_with_stderr_merged_and_a_nonzero_exit_keeps_its_code() {
    let tmp = crate::mktemp_dir("atv-exec").expect("mktemp");
    let r = run(
        &shell(
            "echo out; echo err >&2; echo out2",
            "echo out& (echo err)1>&2& echo out2",
        ),
        env_in(&tmp),
    );
    assert!(r.ok, "{r:?}");
    assert_eq!(said(&r), "out\nerr\nout2");
    let r = run(
        &shell("echo why >&2; exit 3", "(echo why)1>&2& exit 3"),
        env_in(&tmp),
    );
    assert!(!r.ok);
    assert_eq!(said(&r), "why");
    assert_eq!(r.code, Some(3));
    std::fs::remove_dir_all(&tmp).ok();
}

#[test]
fn a_replayed_child_starts_where_and_as_it_was_recorded() {
    let tmp = crate::mktemp_dir("atv-replay").expect("mktemp");
    let pkg = tmp.join("pkg");
    std::fs::create_dir_all(&pkg).expect("mkdir");
    std::fs::write(pkg.join("marker"), b"in-pkg").expect("write");
    let added = [(OsString::from("GATE_ADDED"), OsString::from("gate"))];
    let env = ExecEnv {
        add_env: &added,
        ..env_in(&tmp)
    };
    assert!(
        std::env::var_os(INHERITED).is_some(),
        "the probe needs {INHERITED} where the replay could inherit it"
    );
    let argv = shell_argv(
        &format!(
            "cat marker; printf ' %s %s %s' \"${{RECORDED-unset}}\" \"${{GATE_ADDED-unset}}\" \
             \"${{{INHERITED}-unset}}\""
        ),
        &format!("type marker& echo  %RECORDED% %GATE_ADDED% %{INHERITED}%"),
    );
    let cmd = Cmd::replayed(
        &argv,
        &pkg,
        vec![
            (OsString::from("RECORDED"), OsString::from("yes")),
            (OsString::from("PATH"), OsString::from("/usr/bin:/bin")),
        ],
    )
    .preamble("     Running tests/x.rs (target/debug/deps/x-1)\n");
    let r = run(&cmd, env);
    assert!(r.ok, "{}", r.output);
    assert_eq!(
        said(&r),
        format!(
            "     Running tests/x.rs (target/debug/deps/x-1)\nin-pkg yes {} {}",
            unset("GATE_ADDED"),
            unset(INHERITED)
        ),
        "the preamble first, then the child — in its own directory, with only what \
         was recorded"
    );
    std::fs::remove_dir_all(&tmp).ok();
}

#[test]
fn the_flag_moves_the_ceiling_and_only_a_typed_word_removes_it() {
    for junk in ["", "  ", "45m", "soon", "-5", "NaN", "1e400", "4e9"] {
        assert_eq!(
            parse_ceiling(junk),
            None,
            "{junk:?} is not a number of seconds, and must not disable the ceiling"
        );
    }
    for off in ["0", "0.0", "off", "OFF", "none", "Never", " off "] {
        assert_eq!(
            parse_ceiling(off),
            Some(None),
            "{off:?} must disable the ceiling"
        );
    }
    assert_eq!(parse_ceiling("90"), Some(Some(Duration::from_secs(90))));
    assert_eq!(
        parse_ceiling("0.25"),
        Some(Some(Duration::from_millis(250)))
    );
}

#[cfg(unix)]
const GONE: Duration = Duration::from_secs(60);

#[cfg(unix)]
const HANG_DETECTOR: Duration = Duration::from_secs(60);

#[cfg(unix)]
fn gone_within(pid: &str, window: Duration) -> bool {
    let deadline = Instant::now() + window;
    loop {
        let stat = std::process::Command::new(aterm_uds::os_path!("ps"))
            .args(["-o", "stat=", "-p", pid])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default();
        if stat.is_empty() || stat.starts_with('Z') {
            return true;
        }
        if Instant::now() > deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(unix)]
fn written_pid(file: &Path) -> Option<String> {
    let t = std::fs::read_to_string(file).ok()?;
    t.ends_with('\n').then(|| t.trim().to_string())
}

#[cfg(unix)]
fn read_pid(file: &Path) -> String {
    let deadline = Instant::now() + HANG_DETECTOR;
    loop {
        if let Some(pid) = written_pid(file) {
            return pid;
        }
        assert!(Instant::now() < deadline, "no pid in {}", file.display());
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[cfg(unix)]
fn with_grandchild(file: &Path) -> Cmd {
    Cmd::new("/bin/sh").args([
        "-c",
        &format!("sleep 600 & echo $! > '{}'; wait", file.display()),
    ])
}

#[cfg(unix)]
#[test]
fn a_ceiling_kill_takes_the_childs_whole_process_group() {
    let tmp = crate::mktemp_dir("atv-ceil-group").expect("mktemp");
    let mut ceiling = Duration::from_millis(500);
    let grandchild = loop {
        let pidfile = tmp.join(format!("grandchild-{}ms.pid", ceiling.as_millis()));
        let r = run(&with_grandchild(&pidfile), ceiled(&tmp, Some(ceiling)));
        assert!(
            !r.ok && r.output.contains(crate::findings::CEILING_KILL),
            "{}",
            r.output
        );
        if let Some(pid) = written_pid(&pidfile) {
            break pid;
        }
        assert!(
            ceiling < GONE,
            "no shell backgrounded its grandchild within a {ceiling:?} ceiling"
        );
        ceiling *= 2;
    };
    assert!(
        gone_within(&grandchild, GONE),
        "the grandchild {grandchild} outlived the ceiling kill"
    );
    std::fs::remove_dir_all(&tmp).ok();
}

/// A child of `/bin/sleep 600` leading its own group, under a run in `tmp`.
#[cfg(unix)]
fn a_sleeping_leader(tmp: &Path) -> Child {
    spawn_command(
        &Cmd::new("/bin/sleep").arg("600"),
        env_in(tmp),
        Path::new(TASKPOLICY),
    )
    .spawn()
    .expect("spawn sleep")
}

/// `pid`'s group, `SIGKILL`ed by `/bin/kill` once a test's wait on it ran past its hang
/// detector, so the thread blocked on its reap returns.
#[cfg(unix)]
fn kill_stray_group(pid: u32) {
    let _ = std::process::Command::new("/bin/kill")
        .args(["-KILL", "--", &format!("-{pid}")])
        .status();
}

/// The reap after `SIGKILL` is bounded: a child still there at the bound is left as it is,
/// named, and the wait returns; once it is killed, the same reap takes it.
#[cfg(unix)]
#[test]
fn a_reap_after_sigkill_gives_up_at_its_bound_and_leaves_the_child_as_it_is() {
    let tmp = crate::mktemp_dir("atv-bounded-reap").expect("mktemp");
    let mut child = a_sleeping_leader(&tmp);
    let pid = child.id();
    let (tx, rx) = std::sync::mpsc::channel();
    let reaper = std::thread::spawn(move || {
        let mut live = group::Live::enter(child.id());
        let left = reap_killed(&mut child, &mut live, Duration::from_millis(200));
        let _ = tx.send(left);
        let _ = live.kill();
        reap_killed(&mut child, &mut live, HANG_DETECTOR)
    });
    let Ok(left) = rx.recv_timeout(HANG_DETECTOR) else {
        kill_stray_group(pid);
        let _ = reaper.join();
        panic!("the reap waited past its bound");
    };
    let left = left.expect("a child still there at the bound");
    assert!(left.contains(&format!("pid {pid},")), "{left}");
    assert_eq!(
        reaper.join().expect("the reaper"),
        None,
        "the killed child reaped"
    );
    assert!(gone_within(&pid.to_string(), HANG_DETECTOR));
    std::fs::remove_dir_all(&tmp).ok();
}

/// A ceiling's kill whose `SIGKILL` does not end the leader (a stand-in: the `SIGKILL` a
/// [`group::spared::Scope`] sends nowhere, as macOS's held first exec may not die of it) gives
/// up its reap at its bound and returns, its block naming the leader it left as it is; that
/// leader is still verify's to kill and reap. Negative control: the unbounded reap the kill
/// had (`reap_stage(child, true)`) never returns, which the hang detector fails.
#[cfg(unix)]
#[test]
fn a_ceiling_kill_whose_sigkill_does_not_end_the_leader_names_it_and_returns() {
    let tmp = crate::mktemp_dir("atv-spared-kill").expect("mktemp");
    let mut child = a_sleeping_leader(&tmp);
    let pid = child.id();
    let (tx, rx) = std::sync::mpsc::channel();
    let killer = std::thread::spawn(move || {
        let mut live = group::Live::enter(child.id());
        let note = {
            let _spared = group::spared::Scope::arm();
            kill_and_reap(&mut child, &mut live, None, Duration::from_secs(1))
        };
        let _ = tx.send(note);
        let _ = live.kill();
        reap_killed(&mut child, &mut live, HANG_DETECTOR)
    });
    let Ok(note) = rx.recv_timeout(HANG_DETECTOR) else {
        kill_stray_group(pid);
        let _ = killer.join();
        panic!("the kill's reap waited past its bound");
    };
    assert_eq!(
        note,
        format!("  (it did not end within 1s of SIGKILL: pid {pid}, left as it is)\n")
    );
    assert_eq!(
        killer.join().expect("the killer"),
        None,
        "the leader, killed, reaped"
    );
    assert!(gone_within(&pid.to_string(), HANG_DETECTOR));
    std::fs::remove_dir_all(&tmp).ok();
}

#[cfg(unix)]
fn suite_with_trap(marker: &Path, pidfile: &Path, ignore: bool) -> Cmd {
    Cmd::new("/bin/bash").args([
        "-c",
        &format!(
            "{}trap 'touch {}' EXIT; sleep 600 & echo $! > '{}'; wait",
            if ignore { "trap '' TERM; " } else { "" },
            marker.display(),
            pidfile.display()
        ),
    ])
}

#[cfg(unix)]
#[test]
fn a_graceful_child_runs_its_exit_trap_and_its_grace_is_a_bound() {
    let tmp = crate::mktemp_dir("atv-ceil-grace").expect("mktemp");
    let cases = [
        ("graced", Some(Duration::from_secs(600)), false, true),
        ("ungraced", None, false, false),
        (
            "ignores-term",
            Some(Duration::from_millis(700)),
            true,
            false,
        ),
    ];
    for (name, grace, ignore, trap_runs) in cases {
        let mut ceiling = Duration::from_millis(500);
        let (r, marker, grandchild) = loop {
            let stem = format!("{name}-{}ms", ceiling.as_millis());
            let (marker, pidfile) = (
                tmp.join(format!("{stem}.trap")),
                tmp.join(format!("{stem}.pid")),
            );
            let mut cmd = suite_with_trap(&marker, &pidfile, ignore);
            if let Some(g) = grace {
                cmd = cmd.term_grace(g);
            }
            let r = run(&cmd, ceiled(&tmp, Some(ceiling)));
            if let Some(pid) = written_pid(&pidfile) {
                break (r, marker, pid);
            }
            assert!(
                ceiling < GONE,
                "{name}: no shell backgrounded its sleep within {ceiling:?}"
            );
            ceiling *= 2;
        };
        assert!(
            !r.ok && r.output.contains(crate::findings::CEILING_KILL),
            "{name}: {}",
            r.output
        );
        assert_eq!(
            marker.exists(),
            trap_runs,
            "{name}: the EXIT trap\n{}",
            r.output
        );
        assert!(
            gone_within(&grandchild, GONE),
            "{name}: {grandchild} outlived the kill"
        );
    }
    std::fs::remove_dir_all(&tmp).ok();
}

/// [`group::empty`] asks the kernel only about one real group: a pgid of 0 (to `killpg`, this
/// process's own group), 1 (every process this user may signal), or one past `i32::MAX` is
/// `None`, never asked; a group whose one member is this test's exited, unreaped child (a
/// leader of its own session) is not empty (`0` on Linux, `EPERM` on macOS, which signals no
/// zombie); and once the child is reaped the group is empty. NEGATIVE CONTROLS: let 0 or 1
/// through its filter and each reads `Some(false)`; read `EPERM` as `None` and the zombie's
/// group is unknown on macOS.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn only_a_real_group_is_asked_and_an_unreaped_leader_keeps_it() {
    let past = u32::try_from(i32::MAX).expect("i32::MAX") + 1;
    for pgid in [0, 1, past, u32::MAX] {
        assert_eq!(group::empty(pgid), None, "pgid {pgid}");
    }
    let mut leader = aterm_uds::spawnfd::command("/bin/sh");
    leader
        .args(["-c", "exit 0"])
        .setsid(true)
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .stdout(aterm_uds::spawnfd::Stdio::null())
        .stderr(aterm_uds::spawnfd::Stdio::null());
    let mut leader = leader.spawn().expect("a leader");
    let pid = leader.id();
    aterm_spec::wait::until_within(HANG_DETECTOR, "the leader's exit", || {
        group::exited(pid, false)
            .expect("its exit, unreaped")
            .then_some(())
    });
    assert_eq!(
        group::empty(pid),
        Some(false),
        "the unreaped leader keeps its group"
    );
    leader.wait().expect("the leader, reaped");
    assert_eq!(group::empty(pid), Some(true), "reaped, it holds no one");
}

/// A group whose members have all ended takes a signal without failing: on macOS its `killpg`
/// answers `EPERM`, which [`aterm_uds::process::settle_group_signal`] settles from the listing,
/// and a timed-out run must not report it as a group the kill failed to reach. NEGATIVE CONTROL:
/// return `last_os_error` from `signal_group` unsettled and `kill` fails here on macOS.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_signal_to_a_group_whose_members_ended_is_not_a_failure() {
    let mut leader = aterm_uds::spawnfd::command("/bin/sh");
    leader
        .args(["-c", "exit 0"])
        .setsid(true)
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .stdout(aterm_uds::spawnfd::Stdio::null())
        .stderr(aterm_uds::spawnfd::Stdio::null());
    let mut leader = leader.spawn().expect("a leader");
    let pid = leader.id();
    aterm_spec::wait::until_within(HANG_DETECTOR, "the leader's exit", || {
        group::exited(pid, false)
            .expect("its exit, unreaped")
            .then_some(())
    });
    let live = group::Live::enter(pid);
    live.kill().expect("a group with nothing left to signal");
    live.term().expect("a group with nothing left to signal");
    drop(live);
    leader.wait().expect("the leader, reaped");
}

/// [`group::judged`] on each answer `killpg` gives: `ESRCH` is empty and `0` is not, neither
/// reading the listing; any other errno is unknown; and `EPERM` is not empty, unless on Apple
/// the group's whole listing finds no process, which is all an exec's leftover image holds
/// there after its process is reaped. A listing cut short, none, or one that names a process
/// (a zombie, another user's) keeps `EPERM` not empty. NEGATIVE CONTROL: answer
/// `Some(false)` for every `EPERM` in `group::judged`, as it was, and the leftover's group
/// reads not empty on Apple.
#[cfg(unix)]
#[test]
fn a_group_held_only_by_an_exec_leftover_reads_empty() {
    use aterm_uds::process::GroupMembers;
    let unread = || -> Option<GroupMembers> { panic!("the listing is not read") };
    assert_eq!(group::judged(Err(Some(libc::ESRCH)), unread), Some(true));
    assert_eq!(group::judged(Ok(()), unread), Some(false));
    assert_eq!(group::judged(Err(Some(libc::EINVAL)), unread), None);
    assert_eq!(group::judged(Err(None), unread), None);
    let whole = GroupMembers {
        pids: Vec::new(),
        complete: true,
    };
    assert_eq!(
        group::judged(Err(Some(libc::EPERM)), || Some(whole.clone())),
        Some(cfg!(target_vendor = "apple")),
        "a whole listing that finds no process"
    );
    let cut = GroupMembers {
        pids: Vec::new(),
        complete: false,
    };
    let named = GroupMembers {
        pids: vec![4242],
        complete: true,
    };
    for listing in [Some(cut), Some(named), None] {
        assert_eq!(
            group::judged(Err(Some(libc::EPERM)), || listing.clone()),
            Some(false),
            "{listing:?}"
        );
    }
}

#[cfg(unix)]
#[test]
fn an_interrupted_gate_kills_every_live_stage_group() {
    use std::os::unix::process::ExitStatusExt as _;
    const PIDFILE: &str = "ATV_INTERRUPT_PIDFILE";
    if let Some(pidfile) = std::env::var_os(PIDFILE) {
        group::kill_on_interrupt();
        let pidfile = PathBuf::from(pidfile);
        let armed = pidfile.with_extension("armed");
        let log_path = pidfile.with_extension("log");
        let mut events = crate::event_log::EventLog::unrecorded();
        let mut log = crate::run_log::open(
            Some(&log_path),
            pidfile.parent().unwrap(),
            pidfile.parent().unwrap(),
            events.id(),
        )
        .unwrap();
        events.retain(log.take_events().unwrap());
        log.observe_signals(Some(events)).unwrap();
        let ctx = crate::Ctx::new(
            PathBuf::from("/wt-a"),
            crate::Scope::workspace(),
            crate::EnvSnapshot::default(),
            PathBuf::from("/tmp/atv-s"),
        );
        let records = pidfile.with_extension("records");
        let record = crate::runs::Runs::open(&ctx, &records, |_, _| Run {
            case_observations: None,
            stopped_at: None,
            hung: None,
            ok: false,
            output: String::new(),
            code: None,
            spawn_error: None,
            usage: None,
        });
        if cfg!(any(target_vendor = "apple", target_os = "linux")) {
            assert!(record.is_some(), "the stand-in keeps a record");
        }
        std::thread::scope(|scope| {
            let stage = scope.spawn(|| {
                run(
                    &with_grandchild(&pidfile),
                    ceiled(pidfile.parent().expect("a dir"), None),
                )
            });
            let deadline = Instant::now() + HANG_DETECTOR;
            while !stage.is_finished() && Instant::now() < deadline {
                if group::live_count() > 0 && written_pid(&pidfile).is_some() {
                    std::fs::write(&armed, "armed\n").expect("the armed marker");
                    break;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            let _ = stage.join();
        });
        std::fs::write(
            pidfile.with_extension("origin"),
            aterm_uds::signal_origin::records().join("\n"),
        )
        .unwrap();
        group::end_interrupted();
        return;
    }
    let tmp = crate::mktemp_dir("atv-interrupt").expect("mktemp");
    let me = std::env::current_exe().expect("the test binary");
    let stand_in = |pidfile: &Path| {
        let mut c = std::process::Command::new(&me);
        c.args([
            "--exact",
            "exec::tests::an_interrupted_gate_kills_every_live_stage_group",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(PIDFILE, pidfile)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
        let mut child = c.spawn().expect("the stand-in run starts");
        let armed = pidfile.with_extension("armed");
        let deadline = Instant::now() + HANG_DETECTOR;
        while !armed.exists() {
            if Instant::now() > deadline || matches!(child.try_wait(), Ok(Some(_))) {
                let _ = child.kill();
                let _ = child.wait();
                if let Some(pid) = written_pid(pidfile) {
                    let _ = std::process::Command::new("/bin/kill")
                        .args(["-KILL", &pid])
                        .status();
                }
                panic!("the stand-in run never armed ({})", armed.display());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let grandchild = read_pid(pidfile);
        // SAFETY: the armed marker proves this test's child is running with its handler.
        assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGINT) }, 0);
        (child.wait().expect("the stand-in exits"), grandchild)
    };

    let (status, grandchild) = stand_in(&tmp.join("handled.pid"));
    let origin = std::fs::read_to_string(tmp.join("handled.origin")).unwrap();
    assert!(
        origin.contains(&format!("sender_pid={} ", std::process::id())),
        "{origin}"
    );
    assert!(origin.contains("executable=Some("), "{origin}");
    assert!(origin.contains("argv=Some("), "{origin}");
    let log = std::fs::read_to_string(tmp.join("handled.log")).unwrap();
    let events = std::fs::read_to_string(tmp.join("handled.log.events")).unwrap();
    for text in [&log, &events] {
        assert!(
            text.contains(&format!("sender_pid={} ", std::process::id())),
            "{text}"
        );
    }
    assert!(events.contains("\"event\":\"signal\""), "{events}");
    assert_eq!(
        status.signal(),
        Some(2),
        "it still dies of SIGINT: {status:?}"
    );
    assert!(
        gone_within(&grandchild, GONE),
        "the grandchild {grandchild} outlived the interrupted run"
    );
    if cfg!(any(target_vendor = "apple", target_os = "linux")) {
        let kept = std::fs::read_dir(tmp.join("handled.records"))
            .map(|d| d.flatten().count())
            .unwrap_or(0);
        assert_eq!(kept, 1, "the interrupted run kept its record");
    }
    std::fs::remove_dir_all(&tmp).ok();
}

/// The lock [`lock_waiter_stand_in`] waits on.
#[cfg(unix)]
const WAIT_LOCK: &str = "ATERM_VERIFY_TEST_WAIT_LOCK";

/// A complete fixture line, never a word in a command or checkout path.
#[cfg(unix)]
const LOCK_ADMITTED: &str = "ATERM_VERIFY_TEST_LOCK_ADMITTED";

/// Set by [`lock_waiter_stand_in`]'s `SIGTERM` handler, as a lab's interrupt sets its stop.
#[cfg(unix)]
static STAND_IN_STOPPED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[cfg(unix)]
extern "C" fn stand_in_stop(_: i32) {
    STAND_IN_STOPPED.store(true, Ordering::SeqCst);
}

/// How long [`lock_waiter_stand_in`] takes, once its wait is ended, to say NOT RUN, as a lab
/// lets its claims go first.
#[cfg(unix)]
const STAND_IN_TEARDOWN: Duration = Duration::from_millis(500);

/// Set, [`lock_waiter_stand_in`] waits at a test's pace: a line every tenth of a second, its
/// holder called stuck after [`STUCK_AFTER`] of no progress.
#[cfg(unix)]
const WAIT_PACED: &str = "ATERM_VERIFY_TEST_LOCK_PACED";

/// How long a paced [`lock_waiter_stand_in`] lets its holder make no progress before it calls
/// it stuck.
#[cfg(unix)]
const STUCK_AFTER: Duration = Duration::from_secs(1);

/// Run by the ceiling tests as a stage child: waits for the lock [`WAIT_LOCK`] names, as a lab
/// waits for its lane, says when it has it, then works until it is ended or its run is gone.
/// A `SIGTERM` ends its wait as a lab's interrupt does: NOT RUN, naming the holder, and exit 3.
#[cfg(unix)]
#[test]
fn lock_waiter_stand_in() {
    use aterm_toolchain::named_lock::{Holder, Pace, Refused, Wait, take, take_paced};
    let Some(path) = std::env::var_os(WAIT_LOCK).map(PathBuf::from) else {
        return;
    };
    // SAFETY: `stand_in_stop` is an `extern "C" fn(i32)` that lives for the whole program and
    // only stores to an atomic, which is async-signal-safe.
    unsafe {
        libc::signal(libc::SIGTERM, stand_in_stop as extern "C" fn(i32) as usize);
    }
    let me = Holder::me("lab linux exec", Path::new("/checkouts/waiter"));
    let stop = || STAND_IN_STOPPED.load(Ordering::SeqCst);
    let taken = if std::env::var_os(WAIT_PACED).is_some() {
        let pace = Pace {
            say_every: Duration::from_millis(100),
            stalled_after: STUCK_AFTER,
        };
        take_paced(&path, &me, Wait::Block, &mut |_, _| {}, &stop, pace)
    } else {
        take(&path, &me, Wait::Block, &mut |_, _| {}, &stop)
    };
    let _held = match taken {
        Ok(held) => held,
        Err(Refused::Stopped { holder, .. }) => {
            std::thread::sleep(STAND_IN_TEARDOWN);
            println!(
                "NOT RUN: interrupted while waiting for {}, which holds the lane",
                holder.map_or_else(|| "no one".to_string(), |h| h.describe(0))
            );
            std::process::exit(3);
        }
        Err(e) => panic!("the lock, once its holder lets go: {e:?}"),
    };
    println!("\n{LOCK_ADMITTED}");
    // Its run ends it; a run that died first (a failed test) left it a new parent, and it ends.
    let run = std::os::unix::process::parent_id();
    while std::os::unix::process::parent_id() == run {
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// A stand-in holder's lock at `dir/lane.lock`, and a stage child that waits on it.
#[cfg(unix)]
fn held_lane(dir: &Path) -> (aterm_toolchain::named_lock::NamedLock, Cmd) {
    held_lane_by(dir, std::process::id())
}

/// [`held_lane`], its record naming `pid` as the holder.
#[cfg(unix)]
fn held_lane_by(dir: &Path, pid: u32) -> (aterm_toolchain::named_lock::NamedLock, Cmd) {
    let lock = dir.join("lane.lock");
    let held = aterm_toolchain::named_lock::take(
        &lock,
        &aterm_toolchain::named_lock::Holder {
            pid,
            ..aterm_toolchain::named_lock::Holder::me(
                "lab linux proof",
                Path::new("/checkouts/holder"),
            )
        },
        aterm_toolchain::named_lock::Wait::No,
        &mut |_, _| {},
        &|| false,
    )
    .expect("a free lock");
    let waiter = Cmd::new(std::env::current_exe().expect("the test binary"))
        .args([
            "--exact",
            "exec::tests::lock_waiter_stand_in",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(WAIT_LOCK, &lock);
    (held, waiter)
}

/// The wait the child of a run with `scratch` has marked, once it has: the one-minute hang
/// detector. A mark this test laid ([`StartMark`]) is not the child's.
#[cfg(unix)]
fn marked(scratch: &Path) -> aterm_toolchain::waits::Waiting {
    let deadline = Instant::now() + HANG_DETECTOR;
    loop {
        let found = std::fs::read_dir(scratch)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with("waits."))
            .find_map(|e| {
                aterm_toolchain::waits::waiting(&e.path())
                    .into_iter()
                    .rfind(|w| w.pid != std::process::id())
            });
        if let Some(w) = found {
            return w;
        }
        assert!(Instant::now() < deadline, "the child never marked its wait");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// A wait this test marks in the marks' directory of the one run with `scratch`, laid as soon
/// as the run lays that directory (before or just after it spawns its child, the one-minute
/// hang detector): the child's start, which its ceiling would count as work until the child
/// marks a wait of its own, so a start a loaded machine holds back (an exec, a page-in) is
/// not what its ceiling ends.
#[cfg(unix)]
struct StartMark {
    /// Held for its drop, which lets the mark go.
    _mark: aterm_toolchain::waits::Mark,
}

#[cfg(unix)]
impl StartMark {
    fn lay(scratch: &Path) -> StartMark {
        let deadline = Instant::now() + HANG_DETECTOR;
        loop {
            let dir = std::fs::read_dir(scratch)
                .into_iter()
                .flatten()
                .flatten()
                .find(|e| e.file_name().to_string_lossy().starts_with("waits."));
            if let Some(mark) = dir.and_then(|d| {
                aterm_toolchain::waits::Mark::at(&d.path(), "the test's child, starting").ok()
            }) {
                return StartMark { _mark: mark };
            }
            assert!(
                Instant::now() < deadline,
                "the run never laid its marks' directory"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}

/// CEILING-WAIT's acceptance: a stage child waits on a lock its holder keeps for longer than
/// the child's ceiling, and the ceiling does not end it while it waits; once it has the lock,
/// the ceiling runs, and ends it at that much work, saying how long it waited apart. The
/// child's start is marked a wait ([`StartMark`]) until its own wait is marked, so however
/// late a loaded machine starts it, its ceiling times only the work after its admission.
/// Negative control: timed from its spawn, as before, the ceiling ends it while it waits,
/// never admitted, with no status.
#[cfg(unix)]
#[test]
fn the_ceiling_times_a_childs_work_from_its_admission_not_its_wait() {
    let tmp = crate::mktemp_dir("atv-ceiling-wait").expect("mktemp");
    let (held, waiter) = held_lane(&tmp);
    let ceiling = Duration::from_secs(5);
    let running = {
        let tmp = tmp.clone();
        std::thread::spawn(move || run(&waiter, ceiled(&tmp, Some(ceiling))))
    };
    let start = StartMark::lay(&tmp);
    let wait = marked(&tmp);
    drop(start);
    assert!(
        wait.text.contains(&format!(
            "waiting for pid {} (lab linux proof, /checkouts/holder)",
            std::process::id()
        )),
        "{}",
        wait.text
    );
    // Hold the lane past the ceiling: the condition under test, not a wait for anything.
    std::thread::sleep(ceiling + Duration::from_secs(2));
    assert!(
        !running.is_finished(),
        "the ceiling ended the child while it waited: {:?}",
        running.join().map(|r| r.output)
    );
    drop(held);
    let out = running.join().expect("the run");
    assert!(!out.ok && out.code.is_none(), "{out:?}");
    assert!(
        out.output.lines().any(|line| line == LOCK_ADMITTED),
        "{}",
        out.output
    );
    assert!(
        out.output.contains(crate::findings::CEILING_KILL)
            && out.output.contains("of work, over the 5.0s ceiling")
            && out.output.contains("it also waited"),
        "{}",
        out.output
    );
    // A test run counts the same wait as work: a test stuck in one is a hang the ceiling ends.
    let lane = tmp.join("test-run");
    std::fs::create_dir(&lane).expect("a second lane");
    let (held, test) = held_lane(&lane);
    let r = run(
        &test.counting_waits(),
        ceiled(&tmp, Some(Duration::from_secs(2))),
    );
    assert!(
        r.output.contains(crate::findings::CEILING_KILL)
            && !r.output.lines().any(|line| line == LOCK_ADMITTED)
            && !r.output.contains("it also waited"),
        "{}",
        r.output
    );
    drop(held);
    // The Linux lane's row says so too, not a bare `no exit status`.
    let (_, row) = crate::stages::linux_lane_outcome(out.code, &out.output);
    assert!(
        row.contains("ended at the ceiling")
            && row.contains("of work, over the 5.0s ceiling")
            && row.contains("it also waited"),
        "{row}"
    );
    std::fs::remove_dir_all(&tmp).ok();
}

/// A child the run's end finds waiting says so and names the holder, ended as an interrupt
/// ends it: the run is marked ending first, then the child gets the handler's `SIGTERM`, which
/// ends its wait, so its mark goes while it lives on to say NOT RUN. The run does not signal it
/// again, so the child's own NOT RUN, naming the holder, is kept; the run's reason names the
/// wait its clock last saw; and the Linux lane's row reads NOT RUN with it, the lane named and
/// then the wait, never `killed by a signal, or never spawned`. Negative controls: a look
/// after the end that forgets what the last one found reads `the run was interrupted while
/// this child ran`; a run that ends the child itself cuts the child's NOT RUN; a row read from
/// the exit code alone begins `no exit status — killed by a signal`.
#[cfg(unix)]
#[test]
fn a_child_ended_while_it_waits_says_whom_it_waited_for() {
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
    let tmp = crate::mktemp_dir("atv-ended-waiting").expect("mktemp");
    let (held, waiter) = held_lane(&tmp);
    let (end, seen) = (
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicBool::new(false)),
    );
    let running = {
        let (tmp, end, seen) = (tmp.clone(), Arc::clone(&end), Arc::clone(&seen));
        std::thread::spawn(move || {
            ends::when(end, seen);
            run(&waiter, ceiled(&tmp, Some(DEFAULT_CHILD_CEILING)))
        })
    };
    let wait = marked(&tmp);
    // The racing point, held: the run's clock has seen the wait before the end.
    let deadline = Instant::now() + HANG_DETECTOR;
    while !seen.load(Ordering::SeqCst) {
        assert!(
            Instant::now() < deadline,
            "the run's clock never saw the wait"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    end.store(true, Ordering::SeqCst);
    let pgid = i32::try_from(wait.pid).expect("a pid");
    // SAFETY: two integers; `pgid` is the run's own child, which leads its group and cannot be
    // reaped before it exits, and it waits on a lock this test holds until this signal ends
    // that wait.
    assert_eq!(unsafe { libc::killpg(pgid, libc::SIGTERM) }, 0);
    let out = running.join().expect("the run");
    drop(held);
    assert!(
        !out.output.lines().any(|line| line == LOCK_ADMITTED),
        "{}",
        out.output
    );
    assert!(
        out.output.contains(&format!(
            "NOT RUN: interrupted while waiting for pid {} (lab linux proof, /checkouts/holder",
            std::process::id()
        )),
        "the child was ended before it said why: {}",
        out.output
    );
    let why = out
        .spawn_error
        .clone()
        .expect("the run's end is its reason");
    assert!(
        why.starts_with("the run was interrupted while this child waited")
            && why.contains(&wait.text)
            && why.contains(&format!("pid {}", std::process::id())),
        "{why}"
    );
    let mut r = crate::Report::new("linux");
    r.record_driven(
        &out,
        crate::stages::LINUX_LANE,
        crate::stages::linux_lane_outcome,
    );
    let row = r.render();
    let row = row
        .lines()
        .find(|l| {
            l.starts_with(crate::ladder::Outcome::Fail(crate::ladder::Severity::CouldNotRun).tag())
        })
        .unwrap_or_else(|| panic!("no NOT RUN row: {row}"));
    // The row names the lane, then the wait: the child neither died of a signal of its own
    // nor failed to spawn, so it says neither.
    assert!(
        row.contains(&format!(
            "{} — the run was interrupted while this child waited",
            crate::stages::LINUX_LANE
        )) && row.contains(&wait.text),
        "{row}"
    );
    assert!(
        !row.contains("killed by a signal") && !row.contains("never spawned"),
        "{row}"
    );
    std::fs::remove_dir_all(&tmp).ok();
}

/// A wait its waiter calls stuck ends the child only on a second look in a row: a mark is
/// rewritten in place, and a read between its new record's write and its truncation finds
/// the replaced record's `stuck` line after the new first line, as the record here is laid.
/// A holder in the child's own group is never the reason, however long the wait is stuck.
/// Negative control: a clock that believes one look ends the child on the torn record.
#[cfg(unix)]
#[test]
fn a_stuck_wait_is_believed_on_its_second_look() {
    use aterm_toolchain::waits::{Mark, Stuck};
    let tmp = crate::mktemp_dir("atv-stuck-looks").expect("mktemp");
    let marks = tmp.join("waits");
    std::fs::create_dir(&marks).expect("the marks' dir");
    // The holder: this process, whose group is this test's own.
    let holder = std::process::id();
    // SAFETY: `getpgid(0)` reads this process's group; it signals nothing.
    let group = u32::try_from(unsafe { libc::getpgid(0) }).expect("this process's group");
    let stuck = Stuck {
        holders: vec![holder],
        span: Duration::from_secs(900),
    };
    let mut mark = Mark::at(&marks, "waiting for its holder, which works").expect("a mark");
    let mut clock = Clock::new(Some(&marks), &INERT, std::process::id(), None);
    // The torn record: the new first line, the replaced record's `stuck` line after it.
    let path = std::fs::read_dir(&marks)
        .expect("the marks")
        .flatten()
        .map(|e| e.path())
        .next()
        .expect("the mark's file");
    let torn = format!("{holder}\t1\twaiting for its holder, which works\nstuck\t900\t{holder}\n");
    std::fs::write(&path, &torn).expect("the torn record");
    clock.look();
    assert!(
        clock.waits.iter().any(|w| w.stuck.is_some()),
        "{:?}",
        clock.waits
    );
    assert_eq!(clock.stuck(u32::MAX), None, "one look");
    mark.say("waiting for its holder, which works");
    clock.look();
    assert_eq!(clock.stuck(u32::MAX), None, "the record said anew");
    mark.say_stuck(
        "waiting for its holder, which has made no progress",
        Some(&stuck),
    );
    clock.look();
    assert_eq!(clock.stuck(u32::MAX), None, "its first look");
    clock.look();
    let why = clock.stuck(u32::MAX).expect("its second look");
    assert!(
        why.contains("no progress for 15m00s")
            && why.contains("waiting for its holder, which has made no progress"),
        "{why}"
    );
    assert_eq!(clock.stuck(group), None, "a holder in the child's group");
    drop(mark);
    std::fs::remove_dir_all(&tmp).ok();
}

/// A second stuck mark cannot borrow the first mark's confirmation. Dropping the mark or
/// observing it without a stuck tail also ends its confirmation, while fresh diagnostic
/// text and a growing elapsed span on the same wait do not.
#[cfg(unix)]
#[test]
fn stuck_confirmation_belongs_to_the_same_live_mark() {
    use aterm_toolchain::waits::{Mark, Stuck};
    let tmp = crate::mktemp_dir("atv-stuck-mark").expect("mktemp");
    let marks = tmp.join("waits");
    std::fs::create_dir(&marks).expect("the marks' dir");
    let holder = std::process::id();
    let stuck = Stuck {
        holders: vec![holder],
        span: Duration::from_secs(900),
    };
    let mut first = Mark::at(&marks, "waiting").expect("first mark");
    let mut second = Mark::at(&marks, "waiting").expect("second mark");
    let mut clock = Clock::new(Some(&marks), &INERT, holder, None);
    first.say_stuck("waiting", Some(&stuck));
    clock.look();
    assert_eq!(clock.stuck(u32::MAX), None, "first mark's first look");
    first.say("waiting");
    second.say_stuck("waiting", Some(&stuck));
    clock.look();
    assert_eq!(clock.stuck(u32::MAX), None, "second mark's first look");
    second.say_stuck(
        "diagnostic changed",
        Some(&Stuck {
            span: Duration::from_secs(901),
            ..stuck.clone()
        }),
    );
    clock.look();
    assert!(clock.stuck(u32::MAX).is_some(), "same mark's second look");
    second.say("waiting");
    clock.look();
    assert_eq!(clock.stuck(u32::MAX), None, "not stuck this look");
    second.say_stuck("waiting", Some(&stuck));
    clock.look();
    assert_eq!(clock.stuck(u32::MAX), None, "confirmation must start again");
    clock.look();
    assert!(clock.stuck(u32::MAX).is_some());
    drop(first);
    drop(second);
    clock.look();
    assert_eq!(clock.stuck(u32::MAX), None, "no live mark");
    let mut third = Mark::at(&marks, "waiting").expect("third mark");
    third.say_stuck("waiting", Some(&stuck));
    clock.look();
    assert_eq!(
        clock.stuck(u32::MAX),
        None,
        "empty look cleared confirmation"
    );
    clock.look();
    assert!(clock.stuck(u32::MAX).is_some());
    drop(third);
    std::fs::remove_dir_all(&tmp).ok();
}

/// A wait's replacement or expanded holder set needs its own second look. Ordering,
/// duplicates, diagnostic words and a growing span are not changes to that holder set.
#[cfg(unix)]
#[test]
fn stuck_confirmation_belongs_to_the_same_holder_set() {
    use aterm_toolchain::waits::{Mark, Stuck};
    let tmp = crate::mktemp_dir("atv-stuck-holders").expect("mktemp");
    let marks = tmp.join("waits");
    std::fs::create_dir(&marks).expect("the marks' dir");
    let holder = std::process::id();
    let other = u32::MAX - 1;
    let mut mark = Mark::at(&marks, "waiting").expect("a mark");
    let mut clock = Clock::new(Some(&marks), &INERT, holder, None);
    let mut say = |holders, span| {
        mark.say_stuck("waiting", Some(&Stuck { holders, span }));
    };
    say(vec![holder], Duration::from_secs(900));
    clock.look();
    say(vec![other], Duration::from_secs(900));
    clock.look();
    assert_eq!(clock.stuck(u32::MAX), None, "a different holder");
    clock.look();
    assert!(clock.stuck(u32::MAX).is_some(), "holder confirmed");
    say(vec![holder, other], Duration::from_secs(901));
    clock.look();
    assert_eq!(clock.stuck(u32::MAX), None, "a new holder joined");
    say(vec![other, holder, holder], Duration::from_secs(902));
    clock.look();
    assert!(clock.stuck(u32::MAX).is_some(), "same normalized set");
    say(vec![holder], Duration::from_secs(903));
    clock.look();
    assert_eq!(clock.stuck(u32::MAX), None, "one holder left");
    clock.look();
    assert!(clock.stuck(u32::MAX).is_some());
    drop(mark);
    std::fs::remove_dir_all(&tmp).ok();
}

/// An established wait inside the child's group cannot prime a new external wait. This
/// checks selection when a confirmed but excluded record remains beside the new record.
#[cfg(unix)]
#[test]
fn an_excluded_mark_cannot_confirm_a_new_external_wait() {
    use aterm_toolchain::waits::{Mark, Stuck};
    let tmp = crate::mktemp_dir("atv-stuck-excluded").expect("mktemp");
    let marks = tmp.join("waits");
    std::fs::create_dir(&marks).expect("the marks' dir");
    let holder = std::process::id();
    // SAFETY: `getpgid(0)` reads this process's group; it signals nothing.
    let group = u32::try_from(unsafe { libc::getpgid(0) }).expect("this process's group");
    let mut internal = Mark::at(&marks, "internal wait").expect("internal mark");
    internal.say_stuck(
        "internal wait",
        Some(&Stuck {
            holders: vec![holder],
            span: Duration::from_secs(900),
        }),
    );
    let mut clock = Clock::new(Some(&marks), &INERT, holder, None);
    clock.look();
    clock.look();
    assert_eq!(clock.stuck(group), None, "its holder would be signalled");
    let mut external = Mark::at(&marks, "external wait").expect("external mark");
    // A synthetic PID outside any representable process group, not a process to signal.
    external.say_stuck(
        "external wait",
        Some(&Stuck {
            holders: vec![u32::MAX - 1],
            span: Duration::from_secs(900),
        }),
    );
    clock.look();
    assert_eq!(clock.stuck(group), None, "new external wait seen only once");
    clock.look();
    assert!(clock.stuck(group).is_some(), "external wait seen twice");
    drop(internal);
    drop(external);
    std::fs::remove_dir_all(&tmp).ok();
}

/// Set, the test binary is the idle [`holder_stand_in`].
#[cfg(unix)]
const HOLDER_STAND_IN: &str = "ATERM_VERIFY_TEST_HOLDER";

/// Run as a lock's named holder by [`a_child_waiting_behind_a_stuck_holder_could_not_run`]:
/// lives for twice the hang detector unless its test ends it first, using no CPU time and
/// writing nothing.
#[cfg(unix)]
#[test]
fn holder_stand_in() {
    let Some(_) = std::env::var_os(HOLDER_STAND_IN) else {
        return;
    };
    std::thread::sleep(HANG_DETECTOR * 2);
}

/// A [`holder_stand_in`], killed when dropped: no test leaves one behind.
#[cfg(unix)]
struct HolderStandIn(aterm_uds::spawnfd::Child);

#[cfg(unix)]
impl HolderStandIn {
    fn start() -> HolderStandIn {
        HolderStandIn(
            aterm_uds::spawnfd::command(std::env::current_exe().expect("the test binary"))
                .args([
                    "--exact",
                    "exec::tests::holder_stand_in",
                    "--nocapture",
                    "--test-threads=1",
                ])
                .env(HOLDER_STAND_IN, "1")
                .stdin(aterm_uds::spawnfd::Stdio::null())
                .stdout(aterm_uds::spawnfd::Stdio::null())
                .stderr(aterm_uds::spawnfd::Stdio::null())
                .spawn()
                .expect("the holder stand-in"),
        )
    }

    /// Whether it still runs: no signal reached it.
    fn runs(&mut self) -> bool {
        matches!(self.0.try_wait(), Ok(None))
    }
}

#[cfg(unix)]
impl Drop for HolderStandIn {
    fn drop(&mut self) {
        if self.runs() {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

/// The whole seconds an [`aterm_capacity::span`] under an hour reads (`7s`, `2m05s`).
#[cfg(unix)]
fn span_secs(span: &str) -> Option<u64> {
    let secs = span.strip_suffix('s')?;
    match secs.split_once('m') {
        Some((m, s)) => Some(m.parse::<u64>().ok()? * 60 + s.parse::<u64>().ok()?),
        None => secs.parse().ok(),
    }
}

/// CW-WAIT-UNBOUNDED: a stage child waits on a lock whose named holder lives on and makes no
/// progress. Once its waiter calls the holder stuck, the run ends the child (`SIGTERM`, so it
/// says its own NOT RUN within [`STUCK_GRACE`]) rather than waiting on, inside the hang
/// detector past that grace and with no ceiling that could end it instead; the child could
/// not run, naming the holder and how long it made no progress, at least the waiter's bound;
/// the Linux lane's row reads NOT RUN with it; and the holder is never signalled and still
/// runs. Working-holder classification is covered by the counter-driven tests in
/// `aterm_toolchain::stall`, including a runnable child receiving only a twelfth of a core.
/// No CPU scheduling share is required within a real one-second observation window here.
/// The run has no ceiling, so a starved child start cannot decide the result.
#[cfg(unix)]
#[test]
fn a_child_waiting_behind_a_stuck_holder_could_not_run() {
    let tmp = crate::mktemp_dir("atv-stuck-holder").expect("mktemp");
    let mut holder = HolderStandIn::start();
    let (held, waiter) = held_lane_by(&tmp, holder.0.id());
    let waiter = waiter.env(WAIT_PACED, "1");
    let started = Instant::now();
    let running = {
        let (tmp, waiter) = (tmp.clone(), waiter.clone());
        std::thread::spawn(move || run(&waiter, ceiled(&tmp, None)))
    };
    aterm_spec::wait::until_within(
        STUCK_GRACE + HANG_DETECTOR,
        "the run to end the child",
        || running.is_finished().then_some(()),
    );
    let out = running.join().expect("the run");
    eprintln!(
        "the stuck holder ended the child {:?} after it began",
        started.elapsed()
    );
    let why = out
        .spawn_error
        .clone()
        .expect("the stuck holder is its reason");
    let named = format!("pid {} (lab linux proof, /checkouts/holder)", holder.0.id());
    let figure = why
        .strip_prefix(
            "this child waited for admission or a lock behind a holder that has made no \
             progress for ",
        )
        .and_then(|rest| rest.split_once(", so it was ended"))
        .and_then(|(span, _)| span_secs(span));
    assert!(
        figure.is_some_and(|secs| secs >= STUCK_AFTER.as_secs())
            && why.contains(&format!("{named}, which has held"))
            && why.contains(", and which has made no progress for "),
        "{why}"
    );
    assert!(
        out.output.contains(&format!(
            "NOT RUN: interrupted while waiting for {}",
            named.trim_end_matches(')')
        )),
        "the child was ended before it said why: {}",
        out.output
    );
    assert!(
        !out.output.lines().any(|l| l == LOCK_ADMITTED),
        "{}",
        out.output
    );
    let mut r = crate::Report::new("linux");
    r.record_driven(
        &out,
        crate::stages::LINUX_LANE,
        crate::stages::linux_lane_outcome,
    );
    let row = r.render();
    assert!(
        row.lines().any(|l| l
            .starts_with(crate::ladder::Outcome::Fail(crate::ladder::Severity::CouldNotRun).tag())
            && l.contains(&format!(
                "{} — this child waited for admission or a lock behind a holder",
                crate::stages::LINUX_LANE
            ))
            && l.contains(&named)),
        "{row}"
    );
    assert!(holder.runs(), "the holder was signalled");
    assert_eq!(
        aterm_toolchain::named_lock::read(&tmp.join("lane.lock")).map(|h| h.pid),
        Some(holder.0.id()),
        "the holder keeps the lock"
    );
    drop((held, holder));

    std::fs::remove_dir_all(&tmp).ok();
}

/// A host whose every read of a group finds one member, the group's leader, whose counters
/// have grown by 1 ms of CPU, 2 ms runnable and 1 MiB held at each read so far, whatever
/// the group: how many reads a child gets is the clock's affair, and its last is its most.
#[cfg(unix)]
#[derive(Debug, Default)]
struct Counting {
    reads: AtomicU64,
}

#[cfg(unix)]
impl Host for Counting {
    fn group_usage(&self, pgid: u32) -> Option<Vec<aterm_sysprobe::MemberUsage>> {
        let n = self.reads.fetch_add(1, Ordering::SeqCst) + 1;
        Some(vec![aterm_sysprobe::MemberUsage {
            pid: pgid,
            start: 1,
            cpu_ns: n * 1_000_000,
            runnable_ns: n * 2_000_000,
            footprint: n << 20,
            peak_footprint: n << 20,
            ..aterm_sysprobe::MemberUsage::default()
        }])
    }

    fn idle(&self) -> Option<Duration> {
        None
    }

    fn cores(&self) -> u32 {
        1
    }
}

/// A STAGE'S CHILDREN SUM TO THEIR STAGE (A1): two children run one after the other under one
/// stage's meter each return the use their own reads found, booked on exactly the reads that
/// book the stage (the clock's, then the sweep's of the ended leader, the last and most), so
/// their CPU sums to the stage's observed CPU to the nanosecond, whatever number of reads each
/// got. Each child's CPU is its last read's, its runnable time twice that, its peak that
/// read's footprint. NEGATIVE CONTROL: a sweep that books the stage's meter and not the
/// child's own leaves each child its second-to-last read, and the sum falls short by 2 ms.
#[cfg(unix)]
#[test]
fn a_stages_children_sum_to_their_stage() {
    let tmp = crate::mktemp_dir("atv-usage-sum").expect("mktemp");
    let host: &'static Counting = Box::leak(Box::default());
    let stage = crate::memory::Meter::new();
    let _metering = crate::memory::metering(Some(Arc::clone(&stage)));
    let env = ExecEnv {
        host,
        ..env_in(&tmp)
    };
    let child = || {
        let out = run(&Cmd::new("/bin/sh").args(["-c", "exit 0"]), env);
        assert!(out.ok, "{out:?}");
        (
            out.usage.expect("its tree was read"),
            host.reads.load(Ordering::SeqCst),
        )
    };
    let (a, after_a) = child();
    let (b, after_b) = child();
    let ms = Duration::from_millis;
    assert_eq!(
        a.cpu,
        ms(after_a),
        "the first child's last read is its most"
    );
    assert_eq!(b.cpu, ms(after_b), "{b:?}");
    assert_eq!(
        (a.runnable, a.peak_bytes, a.processes),
        (ms(2 * after_a), after_a << 20, 1)
    );
    let observed = stage.observed_cpu().expect("the stage was read");
    assert_eq!(
        a.cpu + b.cpu,
        Duration::from_nanos(observed.nanoseconds),
        "the children sum to their stage"
    );
    assert_eq!(a.processes + b.processes, observed.processes);
    assert!(a.wall > Duration::ZERO && b.wall > Duration::ZERO);
    std::fs::remove_dir_all(&tmp).ok();
}

/// A host with no reading of a child's tree (every platform but macOS) gives its run no
/// usage, never zeros; nor does a child that never started. NEGATIVE CONTROL: the same child
/// on a host that reads its tree has usage.
#[cfg(unix)]
#[test]
fn a_host_with_no_reading_gives_a_run_no_usage() {
    let tmp = crate::mktemp_dir("atv-usage-none").expect("mktemp");
    let child = Cmd::new("/bin/sh").args(["-c", "exit 3"]);
    let out = run(&child, env_in(&tmp));
    assert_eq!((out.ok, out.code, out.usage), (false, Some(3), None));
    let missing = run(&Cmd::new("/no/such/tool"), env_in(&tmp));
    assert!(missing.spawn_error.is_some() && missing.usage.is_none());
    let host: &'static Counting = Box::leak(Box::default());
    let read = run(
        &child,
        ExecEnv {
            host,
            ..env_in(&tmp)
        },
    );
    assert!(
        read.usage.is_some_and(|u| u.cpu > Duration::ZERO),
        "{read:?}"
    );
    std::fs::remove_dir_all(&tmp).ok();
}

/// A stage child that writes its pid to `pidfile` (its shell leads its group), then runs
/// `then`.
#[cfg(unix)]
fn stand_in(pidfile: &Path, then: &str) -> Cmd {
    Cmd::new("/bin/sh").args(["-c", &format!("echo $$ > '{}'; {then}", pidfile.display())])
}

/// A child [`running`] on a thread of its own, and where its run and the time it ended come.
#[cfg(unix)]
type Running = (
    std::thread::JoinHandle<()>,
    std::sync::mpsc::Receiver<(Run, Instant)>,
);

/// `cmd` run on a thread of its own under `ceiling` on `host`.
#[cfg(unix)]
fn running(cmd: Cmd, dir: &Path, ceiling: Duration, host: &'static dyn Host) -> Running {
    let (tx, rx) = std::sync::mpsc::channel();
    let dir = dir.to_path_buf();
    let thread = std::thread::spawn(move || {
        let env = ExecEnv {
            host,
            ..ceiled(&dir, Some(ceiling))
        };
        let out = run(&cmd, env);
        let _ = tx.send((out, Instant::now()));
    });
    (thread, rx)
}

/// The run `running` answers, once its ceiling ends its child ([`ended_at`]).
#[cfg(unix)]
fn ended(running: Running, pidfile: &Path, what: &str) -> Run {
    ended_at(running, pidfile, what).0
}

/// The run `rx` answers and when it ended, once its ceiling ends its child: the one-minute
/// hang detector, past which the child's group (by the pid it wrote) is killed and the test
/// fails.
#[cfg(unix)]
fn ended_at((thread, rx): Running, pidfile: &Path, what: &str) -> (Run, Instant) {
    let Ok(out) = rx.recv_timeout(HANG_DETECTOR) else {
        if let Some(pid) = written_pid(pidfile) {
            let _ = std::process::Command::new("/bin/kill")
                .args(["-KILL", "--", &format!("-{pid}")])
                .status();
        }
        let _ = thread.join();
        panic!("{what}: the ceiling never ended it");
    };
    let _ = thread.join();
    out
}

/// What a [`HeldAt`] host counts: its answers to `unstarted`, `daemon` and `backlog`.
#[cfg(unix)]
#[derive(Clone, Copy, Debug)]
enum Asked {
    Unstarted,
    Daemon,
    Backlog,
}

/// A stand-in host for a child's tree held at macOS's program check: the child's group as one
/// member with no CPU and no I/O, beside `member` when that is a deeper one. Its clock reads
/// `made_us` at a watch's first look and moves `step` at each look after (each `unstarted`),
/// so a test spans the minutes a syspolicyd's grace takes in a few seconds. The held one forked
/// `held` before `made_us` (`None`: nothing unrun), reads unrun from look `unrun_from` on and,
/// when `runs`, at another CPU at each look (it ran between them), and when `stopped`, stopped
/// as a start-suspended spawn reads (`StoppedUnstarted`, never held); it waits under the
/// syspolicyd `daemon` (pid [`HeldAt::DAEMON`]) and the backlog it is told. It counts what it
/// is asked, and keeps the child's pgid.
#[cfg(unix)]
#[derive(Debug)]
struct HeldAt {
    made_us: u64,
    step: Duration,
    held: Option<Duration>,
    unrun_from: u64,
    runs: bool,
    stopped: bool,
    /// A deeper member's pid, executable and chain; `None` holds the leader.
    member: Option<(u32, PathBuf, Option<Vec<PathBuf>>)>,
    daemon: DaemonNow,
    backlog: Backlog,
    asked: [AtomicU64; 3],
    pgid: AtomicU64,
    release_on_second_look: std::sync::Mutex<Option<File>>,
    clock: Option<(PathBuf, Instant, AtomicU64)>,
}

#[cfg(unix)]
impl HeldAt {
    const DAEMON: u32 = 4242;

    /// A leader held `held` before the first look, under a syspolicyd that started
    /// `daemon_at` seconds after that look (before it, when negative; `None`: none runs), its
    /// pool read `pool_full` and `held_since` held programs forked since it started; its
    /// clock moves 11 min a look, past the daemon's grace by the second.
    fn leader(
        held: Duration,
        daemon_at: Option<i64>,
        pool_full: Option<bool>,
        held_since: Option<usize>,
    ) -> HeldAt {
        let made_us = aterm_sysprobe::jobs::now_us();
        let daemon = daemon_at.map_or(DaemonNow::NoneRunning, |at| {
            let s = made_us
                .checked_add_signed(at * 1_000_000)
                .expect("in range");
            DaemonNow::Running(aterm_uds::program_check::Daemon {
                pid: Self::DAEMON,
                start_s: s / 1_000_000,
                start_us: u32::try_from(s % 1_000_000).expect("under a second"),
            })
        });
        HeldAt {
            made_us,
            step: Duration::from_secs(11 * 60),
            held: Some(held),
            unrun_from: 0,
            runs: false,
            stopped: false,
            member: None,
            daemon,
            backlog: Backlog {
                pool_full,
                held_since,
            },
            asked: Default::default(),
            pgid: AtomicU64::new(0),
            release_on_second_look: Default::default(),
            clock: None,
        }
    }

    /// The stranded shape (2026-10-29's band): held 31 min, seen held by the first look, under
    /// a syspolicyd that started a second after it and answers alone.
    fn stranded() -> HeldAt {
        Self::leader(Duration::from_secs(31 * 60), Some(1), Some(false), Some(0))
    }

    fn leaked(self) -> &'static HeldAt {
        Box::leak(Box::new(self))
    }

    /// Advance only after the actual shell wrote its execution proof. Until then the
    /// clock stays at its base; the parent's one-minute hang detector bounds that wait.
    fn advancing_after(mut self, proof: PathBuf) -> Self {
        self.clock = Some((proof, Instant::now(), AtomicU64::new(0)));
        self
    }

    fn asked(&self, what: Asked) -> u64 {
        self.asked[what as usize].load(Ordering::SeqCst)
    }

    fn ask(&self, what: Asked) -> u64 {
        self.asked[what as usize].fetch_add(1, Ordering::SeqCst)
    }

    /// The held one's pid in group `pgid`.
    fn held_pid(&self, pgid: u32) -> u32 {
        self.member.as_ref().map_or(pgid, |(pid, _, _)| *pid)
    }
}

#[cfg(unix)]
impl Host for HeldAt {
    fn monotonic(&self) -> Instant {
        match &self.clock {
            Some((proof, began, ticks)) => {
                let ran = std::fs::read_to_string(proof)
                    .ok()
                    .and_then(|line| line.trim().parse::<u32>().ok())
                    .is_some_and(|pid| pid > 0);
                let elapsed = if ran {
                    ticks.fetch_add(100, Ordering::SeqCst)
                } else {
                    0
                };
                *began + Duration::from_millis(elapsed)
            }
            None => Instant::now(),
        }
    }
    fn group_usage(&self, pgid: u32) -> Option<Vec<aterm_sysprobe::MemberUsage>> {
        self.pgid.store(u64::from(pgid), Ordering::SeqCst);
        let held = self.held_pid(pgid);
        let cpu = if self.runs {
            1000 * self.asked(Asked::Unstarted)
        } else {
            0
        };
        let quiet = |pid| aterm_sysprobe::MemberUsage {
            pid,
            start: 1,
            cpu_ns: if pid == held { cpu } else { 0 },
            ..aterm_sysprobe::MemberUsage::default()
        };
        Some(
            std::iter::once(pgid)
                .chain(self.member.as_ref().map(|(pid, _, _)| *pid))
                .map(quiet)
                .collect(),
        )
    }

    fn idle(&self) -> Option<Duration> {
        None
    }

    fn cores(&self) -> u32 {
        1
    }

    fn now_us(&self) -> u64 {
        let looks = u32::try_from(self.asked(Asked::Unstarted)).expect("few looks");
        self.made_us + u64::try_from((self.step * looks).as_micros()).expect("small")
    }

    fn unstarted(&self, pgid: u32, pids: &[u32], now_us: u64) -> Vec<Unstarted> {
        let look = self.ask(Asked::Unstarted);
        if look == 1
            && let Some(release) = self.release_on_second_look.lock().unwrap().as_mut()
        {
            release.write_all(b"observed\n").expect("release the child");
        }
        let Some(held) = self.held else {
            return Vec::new();
        };
        let (pid, exe, chain) = match &self.member {
            None => (pgid, None, Some(Vec::new())),
            Some((pid, exe, chain)) => (*pid, Some(exe.clone()), chain.clone()),
        };
        if look < self.unrun_from || !pids.contains(&pid) {
            return Vec::new();
        }
        let start_us = self.made_us - u64::try_from(held.as_micros()).expect("small");
        vec![Unstarted {
            pid,
            facts: aterm_sysprobe::jobs::StartFacts {
                lone_zero: true,
                user_ns: Some(0),
                program_check_wait: true,
                stopped: self.stopped,
                local: Some(true),
                age_ms: Some(now_us.saturating_sub(start_us) / 1000),
                start_us: Some(start_us),
                ..aterm_sysprobe::jobs::StartFacts::default()
            },
            exe,
            chain,
        }]
    }

    fn daemon(&self) -> DaemonNow {
        self.ask(Asked::Daemon);
        self.daemon
    }

    fn backlog(&self, _: aterm_uds::program_check::Daemon, _: u64) -> Backlog {
        self.ask(Asked::Backlog);
        self.backlog
    }
}

/// 2026-10-29's band: a stage child seen held at its first exec before the syspolicyd running
/// now started, which has then run past the bound on a first exec and answers alone, ends
/// within the hang detector under a 3 h ceiling: unrun, naming its pid and the daemon's, a
/// COULD NOT RUN row and no ceiling finding, its group gone. Negative controls: the ones
/// beside it (a daemon its check may still reach) wait to their ceiling.
#[cfg(unix)]
#[test]
fn a_child_held_since_before_the_running_syspolicyd_ends_could_not_run_within_a_minute() {
    let tmp = crate::mktemp_dir("atv-stranded").expect("mktemp");
    let pidfile = tmp.join("stranded.pid");
    let host = HeldAt::stranded().leaked();
    let out = ended(
        running(
            stand_in(&pidfile, "exec sleep 600"),
            &tmp,
            DEFAULT_CHILD_CEILING,
            host,
        ),
        &pidfile,
        "a child its syspolicyd stranded",
    );
    let pid = host.pgid.load(Ordering::SeqCst);
    assert!(!out.ok && out.code.is_none(), "{out:?}");
    let why = out.spawn_error.as_deref().expect("ended unrun");
    assert!(
        why.contains(&format!("(pid {pid})")) && why.contains(&format!("(pid {})", HeldAt::DAEMON)),
        "{why}"
    );
    assert_eq!(out.environment_failure().as_deref(), Some(why));
    assert!(
        !out.output.contains(crate::findings::CEILING_KILL),
        "{}",
        out.output
    );
    let mut r = crate::Report::new("stranded");
    r.decide_child(&out, "band");
    assert!(
        r.outcomes()
            .all(|(o, _)| o == crate::Outcome::Fail(crate::Severity::CouldNotRun)),
        "{}",
        r.render()
    );
    assert!(gone_within(&pid.to_string(), HANG_DETECTOR), "pid {pid}");
    std::fs::remove_dir_all(&tmp).ok();
}

/// The work clock must use its injected monotonic source even after usage disappears.
/// Replacing it with Instant::now loses the planted hour and fails this assertion.
#[cfg(unix)]
#[test]
fn the_work_clock_advances_from_its_host_without_sleeping() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let proof = dir.path().join("proof");
    std::fs::write(&proof, b"42\n").unwrap();
    let host = HeldAt::stranded().advancing_after(proof);
    let mut clock = Clock::new(None, &host, 1, None);
    clock.usage = None;
    host.clock
        .as_ref()
        .unwrap()
        .2
        .store(3_600_000, Ordering::SeqCst);
    clock.tick();
    assert!(clock.worked >= Duration::from_secs(3599));
}

/// Negative controls for the stranded rule, each a leader held 31 min under a 9 s ceiling,
/// over which its watch looks twice, 11 min apart on its host's clock: a syspolicyd older than
/// its first look (its check went to the one running now); one that started after it but has
/// run only 9 min by the second, short of the bound on a first exec (a check queued for it
/// may still be answered); a full pool, an unread pool, a backlog (a held program forked
/// since that daemon started); no syspolicyd and an unread one; a leader first read unrun at
/// the second look (its fork predates the daemon, but its exec, in the same pid, does not);
/// one whose CPU moved between the looks (it ran, and may have exec'd again); a leader that
/// reads stopped before its first instruction (`StoppedUnstarted`, never held, so the daemon
/// is never asked of it). Each waits to its ceiling and ends there unrun, not as a finding,
/// with the leader still unstarted, its words saying how. The deadline clock advances only
/// after the actual child wrote its pid, so these cases do not wait nine real seconds.
/// The backlog is read only once the
/// leader's check was sent before a daemon past its grace. A tree with nothing unrun ends at
/// its ceiling as the finding it was.
#[cfg(unix)]
#[test]
fn a_held_child_its_syspolicyd_may_still_answer_is_waited_for_to_its_ceiling() {
    let tmp = crate::mktemp_dir("atv-held-waits").expect("mktemp");
    let ceiling = Duration::from_secs(9);
    let held = Duration::from_secs(31 * 60);
    let leader = |at, pool_full, held_since| HeldAt::leader(held, at, pool_full, held_since);
    let mut in_grace = HeldAt::stranded();
    in_grace.step = Duration::from_secs(9 * 60);
    let mut unread = HeldAt::stranded();
    unread.daemon = DaemonNow::Unread;
    let mut exec_later = HeldAt::stranded();
    exec_later.unrun_from = 1;
    let mut ran = HeldAt::stranded();
    ran.runs = true;
    let mut nothing = HeldAt::stranded();
    nothing.held = None;
    let mut stopped = HeldAt::stranded();
    stopped.stopped = true;
    let cases = [
        (
            "an older daemon",
            leader(Some(-32 * 60), Some(false), Some(0)),
            false,
        ),
        ("a daemon short of its grace", in_grace, false),
        ("a full pool", leader(Some(1), Some(true), Some(0)), true),
        ("an unread pool", leader(Some(1), None, Some(0)), true),
        ("a backlog", leader(Some(1), Some(false), Some(1)), true),
        ("no daemon", leader(None, Some(false), Some(0)), false),
        ("an unread daemon", unread, false),
        ("an exec after the daemon started", exec_later, false),
        ("a CPU that moved", ran, false),
        ("a leader stopped before it ran", stopped, false),
        ("nothing unrun", nothing, false),
    ]
    .map(|(what, host, backlog)| {
        let pidfile = tmp.join(format!("{what}.pid"));
        let host = host.advancing_after(pidfile.clone()).leaked();
        let run = running(stand_in(&pidfile, "exec sleep 600"), &tmp, ceiling, host);
        (what, host, backlog, pidfile, run)
    });
    for (what, host, backlog, pidfile, run) in cases {
        let out = ended(run, &pidfile, what);
        assert!(
            u128::from(host.clock.as_ref().unwrap().2.load(Ordering::SeqCst))
                >= ceiling.as_millis(),
            "{what}: ended before its ceiling: {out:?}"
        );
        assert!(!out.ok && out.code.is_none(), "{what}: {out:?}");
        assert!(host.asked(Asked::Unstarted) >= 3, "{what}: {host:?}");
        if host.held.is_none() {
            assert!(
                out.spawn_error.is_none() && out.output.contains(crate::findings::CEILING_KILL),
                "{what}: {out:?}"
            );
            continue;
        }
        let why = out
            .spawn_error
            .as_deref()
            .expect("ended unrun at its ceiling");
        assert!(!why.contains("stranded"), "{what}: {why}");
        assert_eq!(
            why.contains("it read stopped"),
            host.stopped,
            "{what}: {why}"
        );
        if host.stopped {
            assert_eq!(host.asked(Asked::Daemon), 0, "{what}: never held");
        }
        assert!(
            !out.output.contains(crate::findings::CEILING_KILL),
            "{what}: {}",
            out.output
        );
        assert_eq!(
            host.asked(Asked::Backlog) > 0,
            backlog,
            "{what}: backlog asked {} times",
            host.asked(Asked::Backlog)
        );
    }
    std::fs::remove_dir_all(&tmp).ok();
}

/// The slow, healthy first exec: a leader held 9 min 59 s at the first look, seen held past
/// the 10 min line by the second, under a syspolicyd older than it whose pool answers, runs
/// on and passes. Its start is asked again, its backlog never.
#[cfg(unix)]
#[test]
fn a_slow_first_exec_under_the_running_syspolicyd_runs_on_and_passes() {
    let tmp = crate::mktemp_dir("atv-slow-exec").expect("mktemp");
    let pidfile = tmp.join("slow.pid");
    let fifo = tmp.join("observed.fifo");
    let path = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
    // SAFETY: `path` owns the NUL-terminated name throughout the call.
    assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
    // Opening both ends cannot block on a child that has not started yet. The release line
    // remains buffered until that child reads it, however late it starts.
    let release = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&fifo)
        .unwrap();
    let host = HeldAt::leader(
        Duration::from_secs(10 * 60 - 1),
        Some(-20 * 60),
        Some(false),
        Some(0),
    );
    *host.release_on_second_look.lock().unwrap() = Some(release);
    let host = host.leaked();
    let out = ended(
        running(
            stand_in(&pidfile, &format!("read observed < '{}'", fifo.display())),
            &tmp,
            DEFAULT_CHILD_CEILING,
            host,
        ),
        &pidfile,
        "a slow first exec",
    );
    assert!(out.ok && out.spawn_error.is_none(), "{out:?}");
    assert!(host.asked(Asked::Unstarted) >= 2, "{host:?}");
    assert_eq!(host.asked(Asked::Backlog), 0, "{host:?}");
    host.release_on_second_look.lock().unwrap().take();
    std::fs::remove_dir_all(&tmp).ok();
}

/// A held deeper member, as cargo's `build-script-build` under `cargo` and `targo`.
#[cfg(unix)]
fn member_of(mut host: HeldAt, chain: &[&str]) -> &'static HeldAt {
    host.member = Some((
        999_999,
        PathBuf::from("/r/target/debug/build/x-1/build-script-build"),
        Some(chain.iter().map(PathBuf::from).collect()),
    ));
    host.leaked()
}

#[cfg(unix)]
const UNDER_CARGO: [&str; 2] = ["/store/bin/cargo", "/store/bin/targo"];

/// Only a member no test binary started is its child's to judge: a deeper member stranded
/// under a test binary (`deps/`) is that test's to bound, so its child, whose leader started,
/// ends at its ceiling as the finding it was; the same member under cargo ends unrun as the
/// stranded leader does, named by its own executable.
#[cfg(unix)]
#[test]
fn a_member_under_a_test_binary_is_the_tests_to_bound() {
    let tmp = crate::mktemp_dir("atv-stranded-member").expect("mktemp");
    // Independent groups: the negative control's ceiling can elapse while the positive
    // control waits for its second program-check observation.
    let under_cargo = tmp.join("under-cargo.pid");
    let cargo_run = running(
        stand_in(&under_cargo, "exec sleep 600"),
        &tmp,
        DEFAULT_CHILD_CEILING,
        member_of(HeldAt::stranded(), &UNDER_CARGO),
    );
    let under_test = tmp.join("under-test.pid");
    let out = ended(
        running(
            stand_in(&under_test, "exec sleep 600"),
            &tmp,
            Duration::from_secs(9),
            member_of(HeldAt::stranded(), &["/r/target/debug/deps/band-1"]),
        ),
        &under_test,
        "a member under a test binary",
    );
    let cargo_out = ended(cargo_run, &under_cargo, "a member under cargo");
    assert!(
        out.spawn_error.is_none() && out.output.contains(crate::findings::CEILING_KILL),
        "{out:?}"
    );
    let out = cargo_out;
    let why = out.spawn_error.as_deref().expect("ended unrun");
    assert!(
        why.contains("build-script-build")
            && why.contains("(pid 999999)")
            && why.contains(&format!("(pid {})", HeldAt::DAEMON)),
        "{why}"
    );
    std::fs::remove_dir_all(&tmp).ok();
}

/// At its ceiling, a member under cargo that its watch has seen held for the bound on a first
/// exec (11 min by the look the ceiling takes) decided nothing, though its leader started, so
/// the child ends unrun, naming it, with no ceiling finding: its syspolicyd is older than its
/// first exec, so it was never stranded. Negative controls: the same member seen held 1 min
/// (a first exec within the bound is work in line), and one under a test binary, end at the
/// ceiling as the finding they were.
#[cfg(unix)]
#[test]
fn a_member_held_past_the_first_exec_bound_at_its_ceiling_ends_unrun() {
    let tmp = crate::mktemp_dir("atv-held-member").expect("mktemp");
    let older = || {
        HeldAt::leader(
            Duration::from_secs(60),
            Some(-40 * 60),
            Some(false),
            Some(0),
        )
    };
    let mut within = older();
    within.step = Duration::from_secs(60);
    let cases = [
        ("past the bound", member_of(older(), &UNDER_CARGO), true),
        ("within the bound", member_of(within, &UNDER_CARGO), false),
        (
            "under a test binary",
            member_of(older(), &["/r/target/debug/deps/band-1"]),
            false,
        ),
    ]
    .map(|(what, host, unrun)| {
        let pidfile = tmp.join(format!("{what}.pid"));
        let run = running(
            stand_in(&pidfile, "exec sleep 600"),
            &tmp,
            Duration::from_secs(3),
            host,
        );
        (what, host, unrun, pidfile, run)
    });
    for (what, host, unrun, pidfile, run) in cases {
        let out = ended(run, &pidfile, what);
        assert!(!out.ok && out.code.is_none(), "{what}: {out:?}");
        assert_eq!(host.asked(Asked::Backlog), 0, "{what}: never stranded");
        if !unrun {
            assert!(
                out.spawn_error.is_none() && out.output.contains(crate::findings::CEILING_KILL),
                "{what}: {out:?}"
            );
            continue;
        }
        let why = out.spawn_error.as_deref().expect("ended unrun");
        assert!(
            why.contains("build-script-build")
                && why.contains("(pid 999999)")
                && !why.contains("stranded"),
            "{what}: {why}"
        );
        assert!(
            !out.output.contains(crate::findings::CEILING_KILL),
            "{what}: {}",
            out.output
        );
        let mut r = crate::Report::new("held");
        r.decide_child(&out, "targo test --no-run");
        assert!(
            r.outcomes()
                .all(|(o, _)| o == crate::Outcome::Fail(crate::Severity::CouldNotRun)),
            "{what}: {}",
            r.render()
        );
    }
    std::fs::remove_dir_all(&tmp).ok();
}

/// A stranded member under a leader that started ends its child as the stuck holder's end
/// does before the ceiling: its group gets `SIGTERM` first, and the leader's handler runs
/// within its `term_grace`, or [`STUCK_GRACE`] when it has none, before the `SIGKILL`.
/// Negative control: the same leader read as the stranded program itself, which never ran and
/// so has no handler, gets `SIGKILL` alone, its grace notwithstanding.
#[cfg(unix)]
#[test]
fn a_started_leader_over_a_stranded_member_gets_its_term_grace_first() {
    let tmp = crate::mktemp_dir("atv-stranded-grace").expect("mktemp");
    let twenty = Duration::from_secs(20);
    let cases = [
        (
            "a member",
            member_of(HeldAt::stranded(), &UNDER_CARGO),
            Some(twenty),
            true,
        ),
        (
            "a member under a leader with no grace of its own",
            member_of(HeldAt::stranded(), &UNDER_CARGO),
            None,
            true,
        ),
        (
            "the leader",
            HeldAt::stranded().leaked(),
            Some(twenty),
            false,
        ),
    ]
    .map(|(what, host, grace, termed)| {
        let pidfile = tmp.join(format!("{what}.pid"));
        let marker = tmp.join(format!("{what}.term"));
        let cmd = stand_in(
            &pidfile,
            &format!(
                "trap 'echo term > \"{}\"; exit 0' TERM; while :; do sleep 1; done",
                marker.display()
            ),
        );
        let cmd = match grace {
            Some(g) => cmd.term_grace(g),
            None => cmd,
        };
        let run = running(cmd, &tmp, DEFAULT_CHILD_CEILING, host);
        (what, termed, pidfile, marker, run)
    });
    for (what, termed, pidfile, marker, run) in cases {
        let out = ended(run, &pidfile, what);
        let why = out.spawn_error.as_deref().expect("ended unrun");
        assert!(why.contains("stranded"), "{what}: {why}");
        assert_eq!(
            std::fs::read_to_string(&marker).ok().as_deref(),
            termed.then_some("term\n"),
            "{what}: {why}"
        );
        let pid = written_pid(&pidfile).expect("its pid");
        assert!(gone_within(&pid, HANG_DETECTOR), "{what}: pid {pid}");
    }
    std::fs::remove_dir_all(&tmp).ok();
}

/// The scope of the stranded rule: a member's chain judged ([`start::in_scope`]) is read
/// whole and holds no executable run from a `deps` directory, at any depth; the walk up the
/// group ([`start::chain_of`]) reads each parent to the leader, and gives none when a record
/// or a path is unread or a parent is outside the group.
#[test]
fn only_a_member_no_test_binary_started_is_its_childs_to_judge() {
    let p = |s: &str| PathBuf::from(s);
    let cargo = [p("/store/bin/cargo"), p("/store/bin/targo")];
    assert!(start::in_scope(Some(&[])));
    assert!(start::in_scope(Some(&cargo)));
    assert!(start::in_scope(Some(&[p("/deps/bin/cargo")])));
    assert!(!start::in_scope(Some(&[p("/r/target/debug/deps/band-1")])));
    assert!(!start::in_scope(Some(&[
        p("/bin/sh"),
        p("/r/target/debug/deps/band-1"),
        p("/store/bin/cargo"),
    ])));
    assert!(!start::in_scope(None));
    // pid: (parent, group); 10 leads group 10, 30 is in group 30.
    let record = |pid| match pid {
        10 => Some((1, 10)),
        11 => Some((10, 10)),
        12 => Some((11, 10)),
        13 => Some((30, 10)),
        14 => Some((15, 10)),
        30 => Some((1, 30)),
        _ => None,
    };
    let path = |pid| match pid {
        10 => Some(p("/store/bin/targo")),
        11 => Some(p("/store/bin/cargo")),
        _ => None,
    };
    let chain = |pid| start::chain_of(pid, 10, record, path);
    assert_eq!(chain(10), Some(Vec::new()));
    assert_eq!(
        chain(12),
        Some(vec![p("/store/bin/cargo"), p("/store/bin/targo")])
    );
    assert_eq!(chain(13), None, "a parent outside the group");
    assert_eq!(chain(14), None, "an unread parent");
    let unread_path = |pid| (pid != 11).then(|| p("/x"));
    assert_eq!(start::chain_of(12, 10, record, unread_path), None);
}

/// The live host reads this test process as started, and the syspolicyd running now as one
/// with a pid, or none, or unread where a loaded Mac holds the table past its bound.
#[cfg(target_os = "macos")]
#[test]
fn the_live_host_reads_this_process_started_and_a_daemon_or_none() {
    let me = std::process::id();
    let pgid = aterm_sysprobe::kproc::record(me)
        .and_then(|r| u32::try_from(r.pgid).ok())
        .expect("this process's record");
    assert_eq!(
        LIVE_HOST.unstarted(pgid, &[me], aterm_sysprobe::jobs::now_us()),
        Vec::new()
    );
    match LIVE_HOST.daemon() {
        DaemonNow::Running(d) => assert!(d.pid > 1, "{d:?}"),
        // A loaded Mac can hold the whole-table read past its bound (`kproc`'s `TABLE_WAIT`),
        // and Unread is the host's answer then: what a read names is held, not that one lands.
        DaemonNow::NoneRunning | DaemonNow::Unread => {}
    }
}

/// CEILING-WAIT's wait for a core: a stand-in child whose tree is given 5% of the time it is
/// runnable, under a 3 s ceiling, still runs 7 s in, whether its host books that time as it
/// passes or in lumps a second apart, twice its reads' window. Given every core it asks for,
/// the ceiling ends it at 3 s of work, and the kill line names apart the time it waited for a
/// core. Negative controls: the same child on a host with no reading of its tree (the clock
/// before this step) is ended at its ceiling while it waits, naming no core; a clock that
/// judges each read alone ends the lumpy one while it is starved.
#[cfg(unix)]
#[test]
fn a_child_starved_of_cores_is_not_ended_for_the_time_it_waited() {
    let tmp = crate::mktemp_dir("atv-starved").expect("mktemp");
    let ceiling = Duration::from_secs(3);
    let control = tmp.join("control.pid");
    let control_run = running(stand_in(&control, "exec sleep 600"), &tmp, ceiling, &INERT);
    let starved = [
        ("as it passes", Starving::at(50)),
        ("in lumps", Starving::lumpy(50, Duration::from_secs(1))),
    ]
    .map(|(how, host)| {
        let pidfile = tmp.join(format!("starved {how}.pid"));
        let run = running(stand_in(&pidfile, "exec sleep 600"), &tmp, ceiling, host);
        (how, host, pidfile, run)
    });
    // Hold them starved past the ceiling: the condition under test, not a wait for anything.
    std::thread::sleep(ceiling + Duration::from_secs(4));
    let held = starved
        .each_ref()
        .map(|(_, _, _, run)| !run.0.is_finished());
    for (_, host, _, _) in &starved {
        host.give(1000);
    }
    let control_out = ended(control_run, &control, "the control");
    for ((how, _, pidfile, run), held) in starved.into_iter().zip(held) {
        let out = ended(run, &pidfile, &format!("booked {how}, given every core"));
        assert!(
            held,
            "the ceiling ended a child starved {how}: {}",
            out.output
        );
        assert!(!out.ok && out.code.is_none(), "{how}: {out:?}");
        assert!(
            out.usage.is_some_and(|u| u.starved > Duration::ZERO),
            "{how}: its usage names the wait for a core: {:?}",
            out.usage
        );
        assert!(
            out.output.contains(crate::findings::CEILING_KILL)
                && out.output.contains("of work, over the 3.0s ceiling")
                && out
                    .output
                    .contains("for a core, which the ceiling leaves out")
                && !out.output.contains("admission or a lock"),
            "{how}: {}",
            out.output
        );
    }
    let out = control_out;
    assert!(
        out.output.contains("of work, over the 3.0s ceiling")
            && !out.output.contains("for a core")
            && out.usage.is_none(),
        "{}",
        out.output
    );
    std::fs::remove_dir_all(&tmp).ok();
}

/// A tree runnable all the time but given no core at all stays idle: after
/// `IDLE_HANG` its time counts as work again, so a 1 s ceiling ends it, naming the time
/// it waited for a core before that apart. Negative control: with no such bound the ceiling
/// never ends it, and the hang detector fails the test.
#[cfg(unix)]
#[test]
fn a_tree_given_no_core_at_all_is_a_hang_its_ceiling_ends() {
    let tmp = crate::mktemp_dir("atv-no-core").expect("mktemp");
    let pidfile = tmp.join("no-core.pid");
    let out = ended(
        running(
            stand_in(&pidfile, "exec sleep 600"),
            &tmp,
            Duration::from_secs(1),
            Starving::at(0),
        ),
        &pidfile,
        "a tree given no core",
    );
    assert!(
        out.output.contains("of work, over the 1.0s ceiling")
            && out
                .output
                .contains("for a core, which the ceiling leaves out"),
        "{}",
        out.output
    );
    std::fs::remove_dir_all(&tmp).ok();
}

/// On the OS's reading of its tree, a stopped child is runnable for none of its time, so its
/// ceiling ends it as a hang; and a busy-looping child, whatever share of the cores it gets,
/// still ends at its ceiling of work. Negative controls: a clock that counts all of a tree's
/// runnable time as waiting never ends the busy one; one that counts a window its tree was
/// runnable for none of as a wait ends the stopped one only at `IDLE_HANG`, and with
/// that bound gone too never ends it (the arithmetic test fails on the first alone); the hang
/// detector fails each that never ends.
#[cfg(unix)]
#[test]
fn a_stopped_child_and_a_busy_one_still_end_at_their_ceiling() {
    let tmp = crate::mktemp_dir("atv-stopped-busy").expect("mktemp");
    for (name, then) in [
        ("stopped", "kill -STOP $$"),
        ("busy", "while :; do :; done"),
    ] {
        let pidfile = tmp.join(format!("{name}.pid"));
        let out = ended(
            running(
                stand_in(&pidfile, then),
                &tmp,
                Duration::from_secs(1),
                &LIVE_HOST,
            ),
            &pidfile,
            name,
        );
        assert!(
            !out.ok
                && out.output.contains(crate::findings::CEILING_KILL)
                && out.output.contains("of work, over the 1.0s ceiling"),
            "{name}: {}",
            out.output
        );
    }
    std::fs::remove_dir_all(&tmp).ok();
}

/// The arithmetic of a window: the part of a window a tree that used `used` in it waited for a
/// core. Negative controls are the cases each rule decides.
#[test]
fn a_window_waits_for_a_core_only_while_its_tree_is_runnable_and_not_running() {
    let ms = Duration::from_millis;
    let used = |cpu: u64, runnable: u64| Used {
        cpu: ms(cpu),
        runnable: ms(runnable),
        io: 0,
    };
    let w = ms(1000);
    let waits = |used: Used, want: Duration| {
        let got = core_wait(w, used, 18);
        assert!(
            got.abs_diff(want) < Duration::from_micros(1),
            "{used:?}: {got:?}, not {want:?}"
        );
    };
    // One thread runnable the whole window, on a core 30% of it: 700 ms waited.
    waits(used(300, 1000), ms(700));
    // Forty threads on 18 cores alone, getting nearly all of them: its own crowd, 50 ms.
    waits(used(17_100, 40_000), ms(50));
    // Ten threads given 10%: 900 ms of the window, not the 9 s their waits sum to.
    waits(used(1000, 10_000), ms(900));
    // One thread runnable half the window, on a core half of that: 250 ms, never 500.
    waits(used(250, 500), ms(250));
    // A blocked tree whose timer thread waited 40 µs to run: 40 µs, never the window.
    waits(
        Used {
            cpu: Duration::from_micros(10),
            runnable: Duration::from_micros(50),
            io: 0,
        },
        Duration::from_micros(40),
    );
    // Stopped, or blocked: runnable for none of it.
    waits(used(0, 0), Duration::ZERO);
    // Running all it was runnable: no wait.
    waits(used(1000, 1000), Duration::ZERO);
}

/// A test binary is hung only after both progress generations stay fixed for the complete
/// evidence window. A complete line or any tree CPU restarts it; an unread tree disarms it.
/// NEGATIVE CONTROLS: wall time with unchanged generations convicts at exactly the window,
/// while each changed generation and `None` do not.
#[test]
fn test_progress_requires_no_complete_line_and_no_tree_cpu() {
    let window = Duration::from_secs(180);
    let begun = Instant::now();

    let mut blocked = TestProgress::new(window, begun);
    assert!(
        !blocked.hung(begun, Some(7), Some(11)),
        "the baseline only arms"
    );
    assert!(!blocked.hung(begun + window - Duration::from_nanos(1), Some(7), Some(11)));
    assert!(blocked.hung(begun + window, Some(7), Some(11)));

    let mut lines = TestProgress::new(window, begun);
    assert!(!lines.hung(begun, Some(1), Some(0)));
    assert!(!lines.hung(begun + window, Some(2), Some(0)));
    assert!(!lines.hung(
        begun + window * 2 - Duration::from_nanos(1),
        Some(2),
        Some(0)
    ));
    assert!(lines.hung(begun + window * 2, Some(2), Some(0)));

    let mut cpu = TestProgress::new(window, begun);
    assert!(!cpu.hung(begun, Some(1), Some(4)));
    assert!(!cpu.hung(begun + window, Some(1), Some(5)));
    assert!(!cpu.hung(
        begun + window * 2 - Duration::from_nanos(1),
        Some(1),
        Some(5)
    ));
    assert!(cpu.hung(begun + window * 2, Some(1), Some(5)));

    let mut unread = TestProgress::new(window, begun);
    assert!(!unread.hung(begun, Some(1), Some(0)));
    assert!(!unread.hung(begun + window, Some(1), None));
    assert!(!unread.hung(begun + window * 2, Some(1), Some(0)));
    assert!(unread.hung(begun + window * 3, Some(1), Some(0)));
    assert!(!unread.hung(begun + window * 4, None, Some(0)));
}

/// The complete-tree probe follows parentage, not process groups, and carries reaped CPU in
/// the owning row. An incomplete table or unread counter proves nothing. NEGATIVE CONTROL:
/// the child and grandchild use different process groups from the root but remain present.
#[test]
fn tree_cpu_includes_descendants_in_other_groups_and_fails_open() {
    let process = |pid, ppid, pgid, start, cpu, reaped| aterm_sysprobe::ProcessReading {
        pid,
        ppid,
        pgid,
        start: Some(start),
        cpu_ns: Some(cpu),
        child_cpu_ns: Some(reaped),
        ..aterm_sysprobe::ProcessReading::default()
    };
    let mut sweep = aterm_sysprobe::Sweep {
        procs: vec![
            process(40, 1, 40, 400, 10, 5),
            process(41, 40, 41, 410, 20, 0),
            process(42, 41, 99, 420, 30, 7),
            process(70, 1, 70, 700, 1_000, 0),
        ],
        truncated: false,
    };
    assert_eq!(
        tree_usage_from_sweep(&sweep, 40),
        Some(vec![
            TreeMember {
                pid: 40,
                start: 400,
                cpu_ns: 15,
            },
            TreeMember {
                pid: 41,
                start: 410,
                cpu_ns: 20,
            },
            TreeMember {
                pid: 42,
                start: 420,
                cpu_ns: 37,
            },
        ])
    );
    sweep.procs[0].child_cpu_ns = None;
    assert_eq!(
        tree_usage_from_sweep(&sweep, 40),
        None,
        "an unread cumulative reaped-child counter fails open; short-lived busy children may have escaped the sweep"
    );
    sweep.procs[0].child_cpu_ns = Some(5);
    sweep.truncated = true;
    assert_eq!(tree_usage_from_sweep(&sweep, 40), None);
    sweep.truncated = false;
    sweep.procs[1].cpu_ns = None;
    assert_eq!(tree_usage_from_sweep(&sweep, 40), None);
}

#[cfg(unix)]
const HUNG_CASE_FIXTURE: &str = "ATERM_VERIFY_HUNG_CASE_FIXTURE";
#[cfg(unix)]
const HUNG_CASE_SOCKET: &str = "ATERM_VERIFY_HUNG_CASE_SOCKET";
#[cfg(unix)]
const HUNG_BUSY_WORKER_SOCKET: &str = "ATERM_VERIFY_HUNG_BUSY_WORKER_SOCKET";

/// A real libtest case used by the no-progress controls. Each mode becomes ready by binding
/// its datagram before it stops printing. `blocked` and `resume` wait in the kernel;
/// `busy-parent` consumes CPU until its probe releases it; and `busy-child` waits on another
/// copy of this test binary which consumes the CPU. There is no time-based fixture behavior.
#[cfg(unix)]
#[test]
fn hung_case_fixture() {
    let Some(mode) = std::env::var_os(HUNG_CASE_FIXTURE) else {
        return;
    };
    let path = PathBuf::from(std::env::var_os(HUNG_CASE_SOCKET).expect("fixture socket"));
    match mode.to_str().expect("fixture mode") {
        "blocked" | "resume" => {
            let socket = std::os::unix::net::UnixDatagram::bind(path).expect("bind fixture");
            let mut byte = [0_u8];
            socket.recv(&mut byte).expect("release fixture");
        }
        "busy-parent" => busy_until_datagram(&path),
        "busy-child" => {
            let mut child =
                aterm_uds::spawnfd::command(std::env::current_exe().expect("this test binary"));
            child
                .args([
                    "--exact",
                    "exec::tests::hung_busy_worker_fixture",
                    "--test-threads=1",
                ])
                .env(HUNG_BUSY_WORKER_SOCKET, &path)
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            let mut child = child.spawn().expect("spawn busy descendant");
            assert!(child.wait().expect("wait for busy descendant").success());
        }
        other => panic!("unknown hung fixture mode {other}"),
    }
}

/// The actual busy descendant of [`hung_case_fixture`]. Its output is sent to `/dev/null`,
/// so only its CPU can keep the parent binary alive.
#[cfg(unix)]
#[test]
fn hung_busy_worker_fixture() {
    let Some(path) = std::env::var_os(HUNG_BUSY_WORKER_SOCKET) else {
        return;
    };
    busy_until_datagram(Path::new(&path));
}

#[cfg(unix)]
fn busy_until_datagram(path: &Path) {
    let socket = std::os::unix::net::UnixDatagram::bind(path).expect("bind busy fixture");
    socket.set_nonblocking(true).expect("nonblocking fixture");
    let mut byte = [0_u8];
    loop {
        match socket.recv(&mut byte) {
            Ok(_) => return,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                std::hint::black_box(byte[0]);
            }
            Err(error) => panic!("receive fixture release: {error}"),
        }
    }
}

#[cfg(unix)]
#[derive(Clone, Copy, Debug)]
enum ProgressKind {
    Flat,
    Leader,
    Descendant,
    ResumeAfterSample,
    UnreadCacheAfterSample,
    SampleError,
}

/// An injected complete descendant-tree probe. Its counter is the deterministic clock for
/// the live controls; `sampled_alive` proves sampling happened before the local kill.
#[cfg(unix)]
#[derive(Debug)]
struct ProgressHost {
    kind: ProgressKind,
    reads: AtomicU64,
    root: std::sync::atomic::AtomicU32,
    sampled_alive: std::sync::atomic::AtomicBool,
    sample_calls: AtomicU64,
    fresh_reads: AtomicU64,
    post_sample_cpu: std::sync::atomic::AtomicBool,
    released: std::sync::atomic::AtomicBool,
    socket: PathBuf,
}

#[cfg(unix)]
impl ProgressHost {
    fn new(kind: ProgressKind, socket: PathBuf) -> Self {
        Self {
            kind,
            reads: AtomicU64::new(0),
            root: std::sync::atomic::AtomicU32::new(0),
            sampled_alive: std::sync::atomic::AtomicBool::new(false),
            sample_calls: AtomicU64::new(0),
            fresh_reads: AtomicU64::new(0),
            post_sample_cpu: std::sync::atomic::AtomicBool::new(false),
            released: std::sync::atomic::AtomicBool::new(false),
            socket,
        }
    }

    fn release(&self) {
        if self.released.swap(true, Ordering::SeqCst) {
            return;
        }
        std::os::unix::net::UnixDatagram::unbound()
            .and_then(|socket| socket.send_to(&[1], &self.socket))
            .expect("release live hang fixture");
    }

    fn reading(&self, root: u32, fresh: bool) -> Option<Vec<TreeMember>> {
        self.root.store(root, Ordering::SeqCst);
        if !self.socket.exists() {
            return None;
        }
        let post_sample_cpu = self.post_sample_cpu.load(Ordering::SeqCst);
        let tick_now = || self.reads.fetch_add(1, Ordering::SeqCst) + 1;
        if matches!(
            self.kind,
            ProgressKind::ResumeAfterSample | ProgressKind::UnreadCacheAfterSample
        ) && self.released.load(Ordering::SeqCst)
        {
            // Released, the child is on its way out, as slowly as a loaded Mac makes it:
            // every read from here on shows progress, so no second finding re-samples it.
            return Some(vec![TreeMember {
                pid: root,
                start: 1,
                cpu_ns: 1 + tick_now(),
            }]);
        }
        if matches!(self.kind, ProgressKind::UnreadCacheAfterSample) && post_sample_cpu && !fresh {
            return None;
        }
        let tick = tick_now();
        let member = |pid, start, cpu_ns| TreeMember { pid, start, cpu_ns };
        let reading = match self.kind {
            ProgressKind::Flat | ProgressKind::SampleError => vec![member(root, 1, 0)],
            ProgressKind::ResumeAfterSample | ProgressKind::UnreadCacheAfterSample => {
                vec![member(root, 1, u64::from(fresh && post_sample_cpu))]
            }
            ProgressKind::Leader => vec![member(root, 1, tick)],
            ProgressKind::Descendant => vec![
                member(root, 1, 0),
                member(root.saturating_add(10_000), 2, tick),
            ],
        };
        if matches!(self.kind, ProgressKind::Leader | ProgressKind::Descendant) && tick >= 4 {
            self.release();
        }
        Some(reading)
    }
}

#[cfg(unix)]
impl Host for ProgressHost {
    fn group_usage(&self, pgid: u32) -> Option<Vec<aterm_sysprobe::MemberUsage>> {
        self.root.store(pgid, Ordering::SeqCst);
        Some(vec![aterm_sysprobe::MemberUsage {
            pid: pgid,
            start: 1,
            cpu_ns: self.reads.load(Ordering::SeqCst),
            ..aterm_sysprobe::MemberUsage::default()
        }])
    }

    fn tree_usage(&self, root: u32) -> Option<Vec<TreeMember>> {
        self.reading(root, false)
    }

    fn fresh_tree_usage(&self, root: u32) -> Option<Vec<TreeMember>> {
        self.fresh_reads.fetch_add(1, Ordering::SeqCst);
        let reading = self.reading(root, true);
        if matches!(
            self.kind,
            ProgressKind::ResumeAfterSample | ProgressKind::UnreadCacheAfterSample
        ) && self.post_sample_cpu.load(Ordering::SeqCst)
        {
            self.release();
        }
        reading
    }

    fn usage_every(&self) -> Duration {
        Duration::ZERO
    }

    fn sample(&self, pid: u32, output_dir: &Path, canceled: &dyn Fn() -> bool) -> StackSample {
        if canceled() {
            return StackSample {
                report: None,
                error: Some("sample canceled".to_owned()),
            };
        }
        self.sample_calls.fetch_add(1, Ordering::SeqCst);
        let alive = i32::try_from(pid)
            .ok()
            .and_then(aterm_uds::process::read_identity)
            .is_some();
        self.sampled_alive.store(alive, Ordering::SeqCst);
        let report = output_dir.join(format!("hung-{pid}.sample.txt"));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        use std::io::Write as _;
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
        if let Err(error) = options
            .open(&report)
            .and_then(|mut file| file.write_all(b"injected all-thread sample\n"))
        {
            return StackSample {
                report: None,
                error: Some(error.to_string()),
            };
        }
        if matches!(
            self.kind,
            ProgressKind::ResumeAfterSample | ProgressKind::UnreadCacheAfterSample
        ) {
            // Ordinary reads deliberately retain the cached zero-CPU view. Only the
            // post-sample fresh read observes this progress, then releases the fixture.
            self.post_sample_cpu.store(true, Ordering::SeqCst);
        }
        StackSample {
            report: Some(report),
            error: matches!(
                self.kind,
                ProgressKind::ResumeAfterSample
                    | ProgressKind::UnreadCacheAfterSample
                    | ProgressKind::SampleError
            )
            .then(|| "injected sampler failure after writing the report".to_owned()),
        }
    }

    fn idle(&self) -> Option<Duration> {
        None
    }

    fn cores(&self) -> u32 {
        1
    }
}

#[cfg(unix)]
fn hung_fixture_command(mode: &str, socket: &Path, hang_after: Duration, stop_at_red: bool) -> Cmd {
    let binary = std::env::current_exe().expect("this test binary");
    let mut command = Cmd::new(&binary)
        .args([
            "-Z",
            "unstable-options",
            "--format",
            "json",
            "--exact",
            "exec::tests::hung_case_fixture",
            "--test-threads=1",
        ])
        .env(HUNG_CASE_FIXTURE, mode)
        .env(HUNG_CASE_SOCKET, socket);
    command.test_cases = Some(CaseWatch {
        spec: "-p aterm-verify --lib".into(),
        binary: binary.display().to_string(),
        json: true,
        stop_at_red,
        hang_after,
    });
    command
}

/// Run a live fixture under an outer one-minute hang detector. That minute is only the
/// test's failure detector: every successful injected decision is immediate. On failure the
/// still-unreaped leader retains its group id, so cleanup can signal exactly that group.
#[cfg(unix)]
fn run_hung_fixture(
    cmd: Cmd,
    dir: &Path,
    host: std::sync::Arc<ProgressHost>,
    red_alive: std::sync::Arc<std::sync::atomic::AtomicBool>,
    red_calls: std::sync::Arc<AtomicU64>,
    fail_case_open: bool,
) -> Run {
    let dir = dir.to_path_buf();
    let (sent, answer) = std::sync::mpsc::channel();
    let thread_host = std::sync::Arc::clone(&host);
    let thread = std::thread::spawn(move || {
        let samples = dir.join("hung-samples");
        let env = ExecEnv {
            host: thread_host.as_ref(),
            hang_samples: Some(&samples),
            ..ceiled(&dir, None)
        };
        let callback_host = std::sync::Arc::clone(&thread_host);
        let callback = || {
            red_calls.fetch_add(1, Ordering::SeqCst);
            let pid = callback_host.root.load(Ordering::SeqCst);
            let alive = i32::try_from(pid)
                .ok()
                .and_then(aterm_uds::process::read_identity)
                .is_some();
            red_alive.store(alive, Ordering::SeqCst);
        };
        let run = if fail_case_open {
            run_test_opening(&cmd, env, &callback, &|_, _| {
                Err(io::Error::other("injected live-capture open failure"))
            })
        } else {
            run_test(&cmd, env, &callback)
        };
        let _ = sent.send(run);
    });
    match answer.recv_timeout(HANG_DETECTOR) {
        Ok(run) => {
            let _ = thread.join();
            run
        }
        Err(error) => {
            let pid = host.root.load(Ordering::SeqCst);
            if let Ok(pid) = i32::try_from(pid)
                && pid > 1
            {
                // SAFETY: the worker retains this exact leader unreaped, so `-pid` still
                // names its owned process group. This is only the outer detector's failure
                // cleanup.
                unsafe {
                    libc::kill(-pid, libc::SIGKILL);
                }
            }
            // Never replace the outer detector with an unbounded join. A killed fixture
            // normally unwinds its worker promptly; if verify itself is wedged, detach it
            // and let this test fail now.
            if answer.recv_timeout(Duration::from_secs(5)).is_ok() {
                let _ = thread.join();
            }
            panic!("the live hang control exceeded its one-minute detector: {error}");
        }
    }
}

/// A blocked real libtest binary is sampled, named with its unfinished case, made the local
/// first red, and killed. NEGATIVE CONTROL: both the sampler and the local halt callback
/// observe its identity alive, proving they precede the kill.
#[cfg(unix)]
#[test]
fn a_blocked_test_binary_is_sampled_and_ends_as_hung() {
    use std::os::unix::fs::PermissionsExt as _;

    let root = aterm_tempfile::tempdir().expect("fixture root");
    let socket = root.path().join("fixture.sock");
    let samples = root.path().join("hung-samples");
    let host = std::sync::Arc::new(ProgressHost::new(ProgressKind::Flat, socket.clone()));
    let red_alive = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let red_calls = std::sync::Arc::new(AtomicU64::new(0));
    let run = run_hung_fixture(
        hung_fixture_command("blocked", &socket, Duration::ZERO, true),
        root.path(),
        std::sync::Arc::clone(&host),
        std::sync::Arc::clone(&red_alive),
        std::sync::Arc::clone(&red_calls),
        false,
    );
    let hung = run.hung.as_ref().expect("a named hung red");
    assert!(!run.ok && run.spawn_error.is_none(), "{run:?}");
    assert_eq!(hung.spec, "-p aterm-verify --lib");
    assert_eq!(
        hung.binary,
        std::env::current_exe().unwrap().display().to_string()
    );
    assert_eq!(hung.window, Duration::ZERO);
    assert_eq!(hung.running, ["exec::tests::hung_case_fixture"]);
    assert!(run.output.contains("HUNG") && run.output.contains("hung_case_fixture"));
    assert!(run.environment_failure().is_none(), "a hang is a tree red");
    let sample = hung.sample.report.as_ref().expect("retained sample");
    assert!(hung.sample.error.is_none(), "{:?}", hung.sample);
    assert!(
        sample.starts_with(&samples) && sample.is_file(),
        "{sample:?}"
    );
    assert_eq!(
        std::fs::metadata(sample).unwrap().permissions().mode() & 0o077,
        0,
        "the raw stack is owner-private"
    );
    assert!(host.sampled_alive.load(Ordering::SeqCst));
    assert_eq!(host.sample_calls.load(Ordering::SeqCst), 1);
    assert!(red_alive.load(Ordering::SeqCst));
    assert_eq!(red_calls.load(Ordering::SeqCst), 1);
}

/// A spawned binary whose live capture cannot be opened is ended immediately as NOT RUN;
/// monitoring is never silently disabled. NEGATIVE CONTROL: the real fixture blocks forever,
/// so converting the injected error to `None` reaches the outer one-minute hang detector.
#[cfg(unix)]
#[test]
fn an_unreadable_live_capture_ends_the_test_binary_unrun() {
    let root = aterm_tempfile::tempdir().expect("fixture root");
    let socket = root.path().join("fixture.sock");
    let host = std::sync::Arc::new(ProgressHost::new(ProgressKind::Flat, socket.clone()));
    let red_alive = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let red_calls = std::sync::Arc::new(AtomicU64::new(0));
    let run = run_hung_fixture(
        hung_fixture_command("blocked", &socket, Duration::ZERO, true),
        root.path(),
        std::sync::Arc::clone(&host),
        std::sync::Arc::clone(&red_alive),
        std::sync::Arc::clone(&red_calls),
        true,
    );
    assert!(!run.ok && run.hung.is_none(), "{run:?}");
    assert!(
        run.spawn_error
            .as_deref()
            .is_some_and(|why| why.contains("cannot monitor")
                && why.contains("injected live-capture open failure")),
        "{run:?}"
    );
    assert!(run.output.contains("NOT RUN"), "{}", run.output);
    assert_eq!(host.sample_calls.load(Ordering::SeqCst), 0);
    assert_eq!(red_calls.load(Ordering::SeqCst), 0);
    assert!(!red_alive.load(Ordering::SeqCst));
}

/// CPU by the binary itself, and CPU by a descendant while the binary waits, both keep it
/// live. The descendant control keeps the root counter flat. NEGATIVE CONTROL: if either
/// progress generation were ignored, the zero evidence window would sample and kill it.
#[cfg(unix)]
#[test]
fn cpu_by_a_test_binary_or_its_busy_child_prevents_a_hung_red() {
    for (mode, kind) in [
        ("busy-parent", ProgressKind::Leader),
        ("busy-child", ProgressKind::Descendant),
    ] {
        let root = aterm_tempfile::tempdir().expect("fixture root");
        let socket = root.path().join("fixture.sock");
        let host = std::sync::Arc::new(ProgressHost::new(kind, socket.clone()));
        let red_alive = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let red_calls = std::sync::Arc::new(AtomicU64::new(0));
        let run = run_hung_fixture(
            hung_fixture_command(mode, &socket, Duration::ZERO, false),
            root.path(),
            std::sync::Arc::clone(&host),
            std::sync::Arc::clone(&red_alive),
            std::sync::Arc::clone(&red_calls),
            false,
        );
        assert!(run.ok && run.hung.is_none(), "{mode}: {run:?}");
        assert_eq!(host.sample_calls.load(Ordering::SeqCst), 0, "{mode}");
        assert_eq!(red_calls.load(Ordering::SeqCst), 0, "{mode}");
        assert!(!red_alive.load(Ordering::SeqCst), "{mode}");
    }
}

/// A tentative no-progress finding is not a verdict: sampling makes CPU progress available
/// while ordinary reads deliberately retain either a cached zero-CPU view or an unread view.
/// Only the independent post-sample sweep observes progress and releases the blocked binary
/// before any kill or red. NEGATIVE CONTROLS: using the cadence cache for confirmation makes
/// the first binary HUNG; letting an unread cached view erase the pre-sample baseline makes
/// the second HUNG. The discarded samples are removed, while each shared run-owned directory
/// stays available to another binary that may already be preparing its own sample.
#[cfg(unix)]
#[test]
fn progress_during_the_stack_sample_cancels_the_hung_finding() {
    for (cache, kind) in [
        ("stale zero", ProgressKind::ResumeAfterSample),
        ("unread", ProgressKind::UnreadCacheAfterSample),
    ] {
        let root = aterm_tempfile::tempdir().expect("fixture root");
        let socket = root.path().join("fixture.sock");
        let samples = root.path().join("hung-samples");
        let host = std::sync::Arc::new(ProgressHost::new(kind, socket.clone()));
        let red_alive = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let red_calls = std::sync::Arc::new(AtomicU64::new(0));
        let run = run_hung_fixture(
            hung_fixture_command("resume", &socket, Duration::ZERO, true),
            root.path(),
            std::sync::Arc::clone(&host),
            std::sync::Arc::clone(&red_alive),
            std::sync::Arc::clone(&red_calls),
            false,
        );
        assert!(run.ok && run.hung.is_none(), "{cache}: {run:?}");
        assert_eq!(host.sample_calls.load(Ordering::SeqCst), 1, "{cache}");
        assert_eq!(
            host.fresh_reads.load(Ordering::SeqCst),
            1,
            "{cache}: confirmation must bypass the shared process-table cache exactly once"
        );
        assert!(
            samples.is_dir(),
            "{cache}: the shared sample directory remains available"
        );
        assert_eq!(
            std::fs::read_dir(&samples)
                .expect("retained sample directory")
                .count(),
            0,
            "{cache}: only this canceled report was discarded"
        );
        assert_eq!(red_calls.load(Ordering::SeqCst), 0, "{cache}");
        assert!(!red_alive.load(Ordering::SeqCst), "{cache}");
    }
}

/// A native sampler can fail after reserving and partly writing its private report. That
/// diagnostic belongs to the hung red rather than being orphaned. NEGATIVE CONTROL: the
/// injected sampler returns an error as well as the report; either half being discarded makes
/// the assertions fail.
#[cfg(unix)]
#[test]
fn a_failed_sampler_retains_and_names_its_partial_report() {
    let root = aterm_tempfile::tempdir().expect("fixture root");
    let socket = root.path().join("fixture.sock");
    let samples = root.path().join("hung-samples");
    let host = std::sync::Arc::new(ProgressHost::new(ProgressKind::SampleError, socket.clone()));
    let red_alive = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let red_calls = std::sync::Arc::new(AtomicU64::new(0));
    let run = run_hung_fixture(
        hung_fixture_command("blocked", &socket, Duration::ZERO, true),
        root.path(),
        std::sync::Arc::clone(&host),
        std::sync::Arc::clone(&red_alive),
        std::sync::Arc::clone(&red_calls),
        false,
    );
    let hung = run.hung.as_ref().expect("a named hung red");
    let report = hung
        .sample
        .report
        .as_ref()
        .expect("the partial sample report");
    assert!(
        report.starts_with(&samples) && report.is_file(),
        "{report:?}"
    );
    assert_eq!(
        hung.sample.error.as_deref(),
        Some("injected sampler failure after writing the report")
    );
    assert!(
        run.output.contains("partial stack sample")
            && run.output.contains("collection failed")
            && run.output.contains(&report.display().to_string()),
        "{}",
        run.output
    );
    assert_eq!(host.sample_calls.load(Ordering::SeqCst), 1);
    assert!(host.sampled_alive.load(Ordering::SeqCst));
    assert_eq!(red_calls.load(Ordering::SeqCst), 1);
    assert!(red_alive.load(Ordering::SeqCst));
}

/// The arithmetic of a span of reads ([`CoreWaitSpan::book`]), 500 ms apart on 18 cores.
/// - One thread given 5% whose time the kernel books every fourth read, in a lump: each lump
///   moves all 1.9 s it waited, and the reads between move nothing. Negative control: judged
///   read by read, the same lumps move 0.475 s each, a quarter of the wait.
/// - Ten seconds of 18 threads on every core, then ten of one thread given 5%: by the end of
///   the second ten the span has moved all 9.5 s of its wait. Negative control: judged on the
///   totals since the first read, the busy past leaves 1 s.
/// - Four threads given 5% that then stop: the reads with nothing booked move nothing, though
///   the span's share alone would call them starved; a booking after them moves their wait.
/// - What the span's reads moved never passes the work counted in them, nor the clock's work;
///   and once forgotten (idle for `IDLE_HANG`), a lump moves at most its own
///   read's work.
#[test]
fn a_span_of_reads_moves_a_wait_the_kernel_books_late() {
    let ms = Duration::from_millis;
    let close = |got: Duration, want: Duration, what: &str| {
        assert!(
            got.abs_diff(want) < Duration::from_micros(10),
            "{what}: {got:?}, not {want:?}"
        );
    };
    let used = |cpu: u64, runnable: u64| Used {
        cpu: ms(cpu),
        runnable: ms(runnable),
        io: 0,
    };
    let w = ms(500);
    let lots = Duration::from_secs(3600);
    let lump = |i: usize| {
        if i % 4 == 3 {
            used(100, 2000)
        } else {
            used(0, 0)
        }
    };

    let mut span = CoreWaitSpan::default();
    let mut total = Duration::ZERO;
    for i in 0..40 {
        let moved = span.book(w, lump(i), w, 18, lots);
        let want = if i % 4 == 3 { ms(1900) } else { Duration::ZERO };
        close(moved, want, &format!("read {i}"));
        total += moved;
    }
    close(total, ms(19_000), "ten lumps");
    let read_by_read: Duration = (0..40).map(|i| core_wait(w, lump(i), 18)).sum();
    close(read_by_read, ms(4750), "the control, read by read");

    let mut span = CoreWaitSpan::default();
    let (busy, starved) = (used(9000, 9000), used(25, 500));
    let mut total = Duration::ZERO;
    for i in 0..40 {
        total += span.book(w, if i < 20 { busy } else { starved }, w, 18, lots);
    }
    close(total, ms(9500), "the starved ten seconds");
    let all = Used {
        cpu: ms(20 * 9000 + 20 * 25),
        runnable: ms(20 * 9000 + 20 * 500),
        io: 0,
    };
    close(
        core_wait(Duration::from_secs(20), all, 18),
        ms(1000),
        "the control, on the totals",
    );

    let mut span = CoreWaitSpan::default();
    for _ in 0..20 {
        span.book(w, used(100, 2000), w, 18, lots);
    }
    for i in 0..5 {
        let moved = span.book(w, used(0, 0), w, 18, lots);
        close(
            moved,
            Duration::ZERO,
            &format!("four threads stopped, read {i}"),
        );
    }
    close(
        span.book(w, used(100, 2000), w, 18, lots),
        ms(6 * 475),
        "four threads given 5% again: a booking covers the reads before it",
    );

    let mut span = CoreWaitSpan::default();
    let moved: Duration = (0..4)
        .map(|i| span.book(w, lump(i), ms(100), 18, lots))
        .sum();
    close(moved, ms(400), "at most the span's work");
    let mut span = CoreWaitSpan::default();
    let moved: Duration = (0..4).map(|i| span.book(w, lump(i), w, 18, ms(200))).sum();
    close(moved, ms(200), "at most the clock's work");
    let mut span = CoreWaitSpan::default();
    for _ in 0..20 {
        span.book(w, starved, w, 18, lots);
    }
    span.clear();
    let moved = span.book(w, used(100, 12_000), w, 18, lots);
    assert!(
        moved <= w && !moved.is_zero(),
        "a lump after a hang: {moved:?}"
    );
}

/// The kill line names a wait the ceiling left out only once it shows as `0.1s` or more, by its
/// own figure; it tells a wait for admission or a lock from one for a core, and names both
/// together.
#[test]
fn a_ceiling_kill_names_the_waits_it_left_out() {
    let s = Duration::from_secs_f64;
    assert_eq!(waited_clause(s(0.049), s(0.049)), "", "neither shows");
    assert!(
        waited_clause(s(0.05), s(0.0)).contains("0.1s"),
        "50 ms shows"
    );
    let lock = waited_clause(s(7.0), s(0.049));
    assert!(lock.contains("7.0s") && !lock.contains("0.0s"), "{lock}");
    let core = waited_clause(s(0.0), s(340.2));
    assert!(core.contains("340.2s") && !core.contains("0.0s"), "{core}");
    let both = waited_clause(s(12.0), s(3.5));
    assert!(both.contains("12.0s") && both.contains("3.5s"), "{both}");
    assert_ne!(
        waited_clause(s(7.0), s(0.0)),
        waited_clause(s(0.0), s(7.0)),
        "which wait it was"
    );
}

#[test]
fn an_in_process_walk_answers_or_is_named_unfinished() {
    assert_eq!(bounded(Some(DEFAULT_CHILD_CEILING), || 7), Ok(7));
    assert_eq!(bounded(None, || 7), Ok(7));
    let panicked = bounded(None, || -> u8 { panic!("a walk that panics") });
    assert_eq!(panicked, Err(Unfinished::Panicked));
    let (_hold, never) = std::sync::mpsc::channel::<()>();
    let ceiling = Duration::from_millis(50);
    let hung = bounded(Some(ceiling), move || never.recv().ok());
    assert_eq!(hung, Err(Unfinished::TimedOut(ceiling)));
}

#[cfg(unix)]
#[test]
fn compiler_progress_child_stand_in() {
    let Some(dir) = std::env::var_os("ATERM_VERIFY_COMPILER_PROGRESS_CHILD") else {
        return;
    };
    let dir = PathBuf::from(dir);
    let capture = include_str!("../tests/fixtures/targo-unit-dispatch-v1.jsonl");
    let dispatch = capture.lines().next().unwrap();
    let artifact = capture.lines().nth(1).unwrap();
    println!("\nBEGIN OWNED COMPILER PAYLOAD\n{dispatch}\n{artifact}");
    eprintln!(
        r#"{{"reason":"compiler-artifact","target":{{"name":"stderr_only","kind":["lib"]}}}}"#
    );
    std::io::stdout().flush().unwrap();
    std::fs::write(dir.join("ready"), b"ready").unwrap();
    let until = Instant::now() + Duration::from_secs(60);
    while !dir.join("release").exists() && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(
        dir.join("release").exists(),
        "compiler fixture was never released"
    );
    println!("{}", dispatch.replace("\"changed-env\"", "\"forced\""));
    println!("exact compiler suffix\nEND OWNED COMPILER PAYLOAD");
    if std::env::var("ATERM_VERIFY_COMPILER_PROGRESS_FAIL").as_deref() == Ok("1") {
        panic!("owned compiler failure after its full payload");
    }
}

#[cfg(unix)]
#[test]
fn compiler_progress_is_observed_before_owned_release_without_final_join_authority() {
    for (step, fails) in [
        (Some("test compile"), false),
        (Some("test compile"), true),
        (None, false),
    ] {
        let dir = aterm_tempfile::tempdir().unwrap();
        let path = dir.path().join("events");
        let events = crate::event_log::EventLog::open(&path).unwrap();
        let cmd = Cmd::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "exec::tests::compiler_progress_child_stand_in",
                "--nocapture",
            ])
            .env(
                "ATERM_VERIFY_COMPILER_PROGRESS_CHILD",
                dir.path().as_os_str(),
            )
            .env(
                "ATERM_VERIFY_COMPILER_PROGRESS_FAIL",
                if fails { "1" } else { "0" },
            );
        let (result, held, (early, early_status)) = std::thread::scope(|scope| {
            let run = scope.spawn(|| {
                run_machine(
                    &cmd,
                    ExecEnv {
                        events: Some(&events),
                        ..ceiled(dir.path(), None)
                    },
                    step,
                )
            });
            let until = Instant::now() + Duration::from_secs(60);
            let early = loop {
                let text = std::fs::read_to_string(&path).unwrap_or_default();
                let observed: Vec<aterm_json::Value> = text
                    .split_inclusive('\n')
                    .filter_map(|line| line.strip_suffix('\n'))
                    .filter_map(|line| line.strip_prefix("verify-event: "))
                    .filter_map(|line| aterm_json::from_str::<aterm_json::Value>(line).ok())
                    .filter(|event| event["event"].as_str() == Some("compile-message-observed"))
                    .collect();
                let both = ["dispatch", "artifact"].iter().all(|kind| {
                    observed
                        .iter()
                        .any(|event| event["message_kind"].as_str() == Some(*kind))
                });
                if (dir.path().join("ready").exists() && (step.is_none() || both))
                    || Instant::now() >= until
                {
                    let status = crate::status::observe(&text);
                    break (text, status);
                }
                std::thread::sleep(Duration::from_millis(1));
            };
            let held = dir.path().join("ready").exists() && !run.is_finished();
            // All assertions happen after the exact owned child was released and joined.
            std::fs::write(dir.path().join("release"), b"release").unwrap();
            (run.join().unwrap(), held, early)
        });
        assert!(held);
        assert_eq!(result.run.ok, !fails, "{}", result.run.output);
        assert_eq!(result.run.code, Some(if fails { 101 } else { 0 }));
        assert!(result.run.stopped_at.is_none());
        let stdout = result.stdout.unwrap();
        let capture = include_str!("../tests/fixtures/targo-unit-dispatch-v1.jsonl");
        let dispatch = capture.lines().next().unwrap();
        let artifact = capture.lines().nth(1).unwrap();
        let payload = stdout
            .split_once("BEGIN OWNED COMPILER PAYLOAD\n")
            .unwrap()
            .1
            .split_once("END OWNED COMPILER PAYLOAD\n")
            .unwrap()
            .0;
        assert_eq!(
            payload,
            format!(
                "{dispatch}\n{artifact}\n{}\nexact compiler suffix\n",
                dispatch.replace("\"changed-env\"", "\"forced\"")
            )
        );
        assert!(!stdout.contains("stderr_only"));
        assert!(result.run.output.contains("stderr_only"));
        if step.is_some() {
            assert!(early.contains("compile-message-observed"));
            assert!(!early.contains("stderr_only"));
            assert!(!early.contains("/Users/"));
            assert!(!early.contains("compile-unit-finished"));
            let rows: Vec<aterm_json::Value> = early
                .lines()
                .map(|line| {
                    aterm_json::from_str(line.strip_prefix("verify-event: ").unwrap()).unwrap()
                })
                .collect();
            assert!(
                rows.iter()
                    .all(|row| row["stage"].as_str() == Some("test compile")
                        && row["provisional"].as_bool() == Some(true)
                        && row["sample_complete"].as_bool() == Some(false))
            );
            assert_eq!(early_status.compiler_message_records, 2);
            assert_eq!(early_status.compiler_progress.len(), 1);
            assert!(early_status.checks.is_empty());
            assert_eq!(early_status.terminal_records, 0);
            let status = early_status.json();
            assert_eq!(
                status["compiler_progress"][0]["latest"]["message_kind"].as_str(),
                Some("artifact")
            );
            assert_eq!(
                status["compiler_progress"][0]["provisional"].as_bool(),
                Some(true)
            );
            assert_eq!(
                status["compiler_progress"][0]["sample_complete"].as_bool(),
                Some(false)
            );
            assert_eq!(early_status.invocations.len(), 1);
            assert_eq!(
                early_status.invocations[0]
                    .observed
                    .compiler_message_records,
                2
            );
        } else {
            assert!(early.is_empty());
            assert!(early_status.compiler_progress.is_empty());
        }
        let final_events = crate::report::compile_unit_events("test compile", &stdout);
        let artifact: aterm_json::Value = final_events
            .iter()
            .filter_map(|line| {
                aterm_json::from_str::<aterm_json::Value>(line.strip_prefix("verify-event: ")?).ok()
            })
            .find(|row| row["event"].as_str() == Some("compile-unit-finished"))
            .unwrap();
        assert!(artifact["rebuild_reason"].is_null());
        assert_eq!(
            artifact["rebuild_reason_status"].as_str(),
            Some("conflicting-dispatch")
        );
    }
}

#[cfg(unix)]
#[test]
fn evidence_child_stand_in() {
    let Some(dir) = std::env::var_os("ATERM_VERIFY_EVIDENCE_CHILD") else {
        return;
    };
    let dir = PathBuf::from(dir);
    if std::env::var("ATERM_VERIFY_EVIDENCE_PREFIX").as_deref() == Ok("1") {
        std::io::stdout().write_all(&vec![b'x'; 33 << 20]).unwrap();
    }
    // Single-threaded libtest leaves its human case prefix unterminated.
    // Start this fixture's synthetic JSON stream on its own line.
    println!();
    println!(
        "{}",
        aterm_json::json!({"type":"test", "event":"started", "name":"observed-outer"})
    );
    println!(
        "{}",
        aterm_json::json!({"type":"test", "event":"failed", "name":"observed-outer", "stdout":"thread 'outer' panicked at src/owned.rs:7:9:\nowned diagnostic"})
    );
    std::io::stdout().flush().unwrap();
    std::fs::write(dir.join("ready"), b"ready").unwrap();
    let until = Instant::now() + Duration::from_secs(60);
    while !dir.join("release").exists() && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(
        dir.join("release").exists(),
        "owned evidence wait was never released"
    );
}

#[cfg(unix)]
#[test]
fn failed_case_evidence_is_durable_while_its_owned_child_is_still_held() {
    for late in [false, true] {
        let dir = aterm_tempfile::tempdir().unwrap();
        let events_path = dir.path().join("events");
        let events = crate::event_log::EventLog::open(&events_path).unwrap();
        let mut documentation = crate::ladder::Report::new("documentation fixture");
        documentation.pass("complete");
        let green = crate::report::judge(
            crate::Check::Doc,
            &[&documentation],
            0.0,
            "",
            String::new(),
            None,
        );
        events.write(&crate::report::check_event(&green, &[&documentation]));
        let mut cmd = Cmd::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "exec::tests::evidence_child_stand_in",
                "--nocapture",
            ])
            .env("ATERM_VERIFY_EVIDENCE_CHILD", dir.path().as_os_str())
            .env("ATERM_VERIFY_EVIDENCE_PREFIX", if late { "1" } else { "0" });
        cmd.test_cases = Some(CaseWatch {
            spec: "owned-test".into(),
            binary: "owned-evidence-child".into(),
            hang_after: TEST_BINARY_HANG,
            json: true,
            stop_at_red: false,
        });
        let (run, (held, early_text, observed, value)) = std::thread::scope(|scope| {
            let run = scope.spawn(|| {
                run(
                    &cmd,
                    ExecEnv {
                        events: Some(&events),
                        ..ceiled(dir.path(), None)
                    },
                )
            });
            let until = Instant::now() + Duration::from_secs(60);
            let early = loop {
                let text = std::fs::read_to_string(&events_path).unwrap_or_default();
                let case = text
                    .split_inclusive('\n')
                    .filter_map(|line| line.strip_suffix('\n'))
                    .filter_map(|line| line.strip_prefix("verify-event: "))
                    .filter_map(|line| aterm_json::from_str::<aterm_json::Value>(line).ok())
                    .find(|event| {
                        event["event"].as_str() == Some("test-case-failed")
                            && event["case"].as_str() == Some("observed-outer")
                    });
                if (case.is_some() && dir.path().join("ready").exists()) || Instant::now() >= until
                {
                    let observed = crate::status::observe(&text);
                    break (text, observed, case);
                }
                std::thread::sleep(Duration::from_millis(1));
            };
            let held = dir.path().join("ready").exists() && !run.is_finished();
            // Release and join before assertions, including the old stderr-only negative.
            std::fs::write(dir.path().join("release"), b"release").unwrap();
            (run.join().unwrap(), (held, early.0, early.1, early.2))
        });
        assert!(held, "child must still own its explicit wait");
        let event = crate::libtest_json::line_event(&early_text);
        assert!(event.is_none(), "verify events are not libtest events");
        let value = value.expect("the actual complete case record was retained before release");
        assert_eq!(value["case"].as_str(), Some("observed-outer"));
        assert!(
            value["output"]
                .as_str()
                .unwrap()
                .contains("src/owned.rs:7:9")
        );
        assert_eq!(value["halts"].as_bool(), Some(false));
        assert_eq!(observed.checks.len(), 1);
        assert_eq!(observed.checks[0].check, "doc");
        assert_eq!(
            observed.checks[0]
                .outcomes
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["ok"]
        );
        assert_eq!(observed.terminal_records, 0);
        assert_eq!(observed.early_cases.len(), 1);
        assert_eq!(observed.early_cases[0].test, "owned-test");
        assert_eq!(
            observed.early_cases[0].case.as_deref(),
            Some("observed-outer")
        );
        assert_eq!(
            observed.early_cases[0]
                .outcomes
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["FAIL"]
        );
        assert!(!observed.json().to_string().contains("src/owned.rs"));
        events.write(&crate::report::run_event(&[green], crate::exit::PASS));
        let completed = crate::status::observe(&std::fs::read_to_string(&events_path).unwrap());
        assert_eq!(completed.early_cases, observed.early_cases);
        assert_eq!(completed.terminal_consistency, "matching-observed-rows");
        assert!(run.ok, "observation does not change an actual zero exit");
        assert!(run.stopped_at.is_none());
        assert_eq!(
            run.case_observations.unwrap().failures[0].case,
            "observed-outer"
        );
    }
}

#[test]
/// A completed stream's final poll reports its failure without granting it permission to stop
/// anything. NEGATIVE CONTROLS: an empty stream and a failed event without its preceding start
/// emit nothing. Direct fixture bytes keep this parser test independent of nested libtest's
/// stdout/stderr write order.
fn a_completed_failure_is_observed_without_forcing_a_stop() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join("capture");
    let mut capture = create_stage_log(&path).unwrap();
    let events_path = dir.path().join("events");
    let events = crate::event_log::EventLog::open(&events_path).unwrap();
    let mut cases = test_progress::LiveCases::open(&path, true).unwrap();
    let watch = CaseWatch {
        spec: "completed".into(),
        binary: "completed-fixture".into(),
        hang_after: TEST_BINARY_HANG,
        json: true,
        stop_at_red: true,
    };

    observe_reaped(&mut cases, &watch, Some(&events), Duration::ZERO);
    assert_eq!(std::fs::read_to_string(&events_path).unwrap(), "");

    let failed = aterm_json::json!({
        "type":"test", "event":"failed", "name":"observed-outer",
        "stdout":"thread 'outer' panicked at src/owned.rs:7:9:\nowned diagnostic"
    });
    writeln!(capture, "{failed}").unwrap();
    capture.flush().unwrap();
    observe_reaped(&mut cases, &watch, Some(&events), Duration::ZERO);
    assert_eq!(std::fs::read_to_string(&events_path).unwrap(), "");

    writeln!(
        capture,
        "{}",
        aterm_json::json!({"type":"test", "event":"started", "name":"observed-outer"})
    )
    .unwrap();
    writeln!(capture, "{failed}").unwrap();
    capture.flush().unwrap();
    observe_reaped(&mut cases, &watch, Some(&events), Duration::ZERO);
    let text = std::fs::read_to_string(events_path).unwrap();
    assert_eq!(text.lines().count(), 1);
    let event: aterm_json::Value =
        aterm_json::from_str(text.trim().strip_prefix("verify-event: ").unwrap()).unwrap();
    assert_eq!(event["case"].as_str(), Some("observed-outer"));
    assert_eq!(event["halts"].as_bool(), Some(false));
    assert!(
        event["output"]
            .as_str()
            .unwrap()
            .contains("owned diagnostic")
    );
}

#[cfg(unix)]
#[test]
fn bounded_output_stand_in() {
    let Some(dir) = std::env::var_os("ATERM_VERIFY_BOUNDED_OUTPUT_CHILD") else {
        return;
    };
    let dir = PathBuf::from(dir);
    println!("owned stdout");
    eprintln!("owned stderr");
    std::fs::write(dir.join("ready"), format!("{}\n", std::process::id())).unwrap();
    if std::env::var_os("ATERM_VERIFY_BOUNDED_OUTPUT_NORMAL").is_some() {
        return;
    }
    while !dir.join("stop").exists() {
        std::thread::sleep(Duration::from_millis(1));
    }
    aterm_uds::process::kill_self_now();
}

#[cfg(unix)]
fn bounded_output_command(dir: &Path, normal: bool) -> aterm_uds::spawnfd::Command {
    let mut command = aterm_uds::spawnfd::command(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "exec::tests::bounded_output_stand_in",
            "--nocapture",
        ])
        .env("ATERM_VERIFY_BOUNDED_OUTPUT_CHILD", dir);
    if normal {
        command.env("ATERM_VERIFY_BOUNDED_OUTPUT_NORMAL", "1");
    }
    command
}

static BOUNDED_QUIET: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

fn bounded_quiet_admit(
    need: aterm_capacity::Need,
    mode: aterm_capacity::Admit,
) -> Result<aterm_capacity::Claim, aterm_capacity::Error> {
    crate::admit_in(
        &aterm_capacity::Ledger::at(BOUNDED_QUIET.get().expect("scratch ledger")),
        &crate::timing_test_machine::Measured::new(&aterm_capacity::Fixed::new(
            aterm_capacity::Reading::of_gib(24, 20, 14),
        )),
        aterm_capacity::Pace {
            poll: Duration::from_millis(20),
            ..aterm_capacity::Pace::default()
        },
        need,
        mode,
        &mut |_| {},
    )
}

/// A completed last stage releases its quiet claim. A last stage whose bounded walk timed
/// out keeps that same claim while the actual worker is blocked, even after the stage's
/// owner is dropped, and releases it once that worker ends. The timeout is injected only
/// after the worker entered; every wait is a hang detector, never an elapsed-time budget.
#[test]
fn a_last_stage_keeps_its_quiet_claim_until_its_bounded_worker_ends() {
    const HANG_DETECTOR: Duration = aterm_spec::wait::MUST_HAPPEN;
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let path = dir.path().join("ledger");
    BOUNDED_QUIET.set(path.clone()).expect("set once");
    let ledger = aterm_capacity::Ledger::at(&path);
    let held = |ledger: &aterm_capacity::Ledger| {
        ledger
            .list()
            .expect("ledger")
            .iter()
            .any(|r| r.held.is_some())
    };
    let ctx = crate::Ctx::new(
        dir.path().to_owned(),
        crate::Scope::crate_only("aterm-gui"),
        crate::EnvSnapshot::default(),
        dir.path().join("scratch"),
    )
    .with_quiet(Some(aterm_capacity::Admit::Wait));
    let last_stage = || {
        std::sync::Mutex::new(crate::Quiet {
            claim: None,
            left: 1,
            admit: bounded_quiet_admit,
        })
    };
    let ordinary = last_stage();
    let worker_ledger = aterm_capacity::Ledger::at(&path);
    let (complete, _) = crate::quietly(&ctx, &ordinary, "ordinary last stage", || {
        bounded(Some(HANG_DETECTOR), move || held(&worker_ledger))
    })
    .expect("admitted");
    assert_eq!(complete, Ok(true), "ordinary worker ran while admitted");
    aterm_spec::wait::until_within(
        HANG_DETECTOR,
        "completed worker released its quiet claim",
        || (!held(&ledger)).then_some(()),
    );

    let (entered, ready) = std::sync::mpsc::channel();
    let (release, resume) = std::sync::mpsc::channel();
    let (finished, done) = std::sync::mpsc::channel();
    let last = last_stage();
    let (answer, _) = crate::quietly(&ctx, &last, "timed-out last stage", || {
        bounded_wait(
            move || {
                entered.send(()).expect("entered");
                resume.recv_timeout(HANG_DETECTOR).expect("released");
                finished.send(()).expect("worker finished");
            },
            |_| {
                ready.recv_timeout(HANG_DETECTOR).expect("worker entered");
                Err(Unfinished::TimedOut(HANG_DETECTOR))
            },
        )
    })
    .expect("admitted");
    drop(last);
    let held_after_stage = held(&ledger);
    release.send(()).expect("release worker");
    done.recv_timeout(HANG_DETECTOR).expect("worker ended");
    aterm_spec::wait::until_within(HANG_DETECTOR, "worker released its quiet claim", || {
        (!held(&ledger)).then_some(())
    });
    assert_eq!(answer, Err(Unfinished::TimedOut(HANG_DETECTOR)));
    assert!(
        held_after_stage,
        "last stage returned but its actual worker still ran"
    );
}

#[cfg(unix)]
#[test]
fn a_bounded_walk_reaps_its_output_child_before_reporting_timeout() {
    let dir = aterm_tempfile::tempdir().unwrap();
    struct Cleanup {
        stop: PathBuf,
        done: std::sync::mpsc::Receiver<std::io::Result<std::process::Output>>,
    }
    impl Drop for Cleanup {
        fn drop(&mut self) {
            std::fs::write(&self.stop, "stop").unwrap();
            let output = self
                .done
                .recv_timeout(HANG_DETECTOR)
                .expect("owned output worker ended");
            if let Ok(output) = output {
                use std::os::unix::process::ExitStatusExt as _;
                assert_eq!(output.status.signal(), Some(9), "stand-in dies by SIGKILL");
            }
        }
    }
    let (done, rx) = std::sync::mpsc::channel();
    let cleanup = Cleanup {
        stop: dir.path().join("stop"),
        done: rx,
    };
    let work = dir.path().to_owned();
    let mut pid = String::new();
    let timed_out: Result<(), _> = bounded_wait(
        move || {
            let output = aterm_uds::output::output(&mut bounded_output_command(&work, false));
            done.send(output).unwrap();
        },
        |_| {
            pid = read_pid(&dir.path().join("ready"));
            assert!(
                aterm_uds::process::pid_alive(pid.parse().unwrap()),
                "actual stand-in is live"
            );
            Err(Unfinished::TimedOut(HANG_DETECTOR))
        },
    );
    assert_eq!(timed_out, Err(Unfinished::TimedOut(HANG_DETECTOR)));
    let absent = !aterm_uds::process::pid_alive(pid.parse().unwrap());
    drop(cleanup);
    assert!(
        absent,
        "bounded returned with its output child {pid} still alive"
    );
}

#[cfg(unix)]
#[test]
fn a_bounded_walk_keeps_normal_child_output() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let work = dir.path().to_owned();
    let output = bounded(Some(HANG_DETECTOR), move || {
        aterm_uds::output::output(&mut bounded_output_command(&work, true))
    })
    .unwrap()
    .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stdout).contains("owned stdout"));
    assert!(String::from_utf8_lossy(&output.stderr).contains("owned stderr"));
}

#[cfg(unix)]
#[test]
fn a_timed_out_walk_cannot_launch_a_child_later() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let work = dir.path().to_owned();
    let (parked, ready) = std::sync::mpsc::channel();
    let (release, resume) = std::sync::mpsc::channel();
    let (sent, result) = std::sync::mpsc::channel();
    let timed_out: Result<(), _> = bounded_wait(
        move || {
            parked.send(()).unwrap();
            resume
                .recv_timeout(HANG_DETECTOR)
                .expect("release the owned walk");
            sent.send(aterm_uds::output::output(&mut bounded_output_command(
                &work, true,
            )))
            .unwrap();
        },
        |_| {
            ready
                .recv_timeout(HANG_DETECTOR)
                .expect("walk parked before launching");
            Err(Unfinished::TimedOut(HANG_DETECTOR))
        },
    );
    assert_eq!(timed_out, Err(Unfinished::TimedOut(HANG_DETECTOR)));
    release.send(()).unwrap();
    let output = result
        .recv_timeout(HANG_DETECTOR)
        .expect("late launch answered");
    assert_eq!(output.unwrap_err().kind(), std::io::ErrorKind::TimedOut);
    assert!(
        !dir.path().join("ready").exists(),
        "the actual child never started"
    );
}

#[cfg(unix)]
#[test]
fn a_timed_out_walk_cannot_launch_from_its_returned_value_drop() {
    struct OutputOnDrop {
        work: PathBuf,
        sent: std::sync::mpsc::Sender<std::io::Result<std::process::Output>>,
    }
    impl Drop for OutputOnDrop {
        fn drop(&mut self) {
            let output = aterm_uds::output::output(&mut bounded_output_command(&self.work, true));
            self.sent.send(output).unwrap();
        }
    }
    let dir = aterm_tempfile::tempdir().unwrap();
    let work = dir.path().to_owned();
    let (parked, ready) = std::sync::mpsc::channel();
    let (release, resume) = std::sync::mpsc::channel();
    let (sent, result) = std::sync::mpsc::channel();
    let timed_out = bounded_wait(
        move || {
            parked.send(()).unwrap();
            resume
                .recv_timeout(HANG_DETECTOR)
                .expect("release the owned walk");
            OutputOnDrop { work, sent }
        },
        |_| {
            ready
                .recv_timeout(HANG_DETECTOR)
                .expect("walk parked before returning");
            Err(Unfinished::TimedOut(HANG_DETECTOR))
        },
    );
    assert!(matches!(timed_out, Err(Unfinished::TimedOut(limit)) if limit == HANG_DETECTOR));
    release.send(()).unwrap();
    let output = result
        .recv_timeout(HANG_DETECTOR)
        .expect("returned value was dropped");
    assert_eq!(output.unwrap_err().kind(), std::io::ErrorKind::TimedOut);
    assert!(
        !dir.path().join("ready").exists(),
        "the actual child never started"
    );
}

#[cfg(unix)]
fn spawned_argv(c: &Command) -> Vec<String> {
    std::iter::once(c.get_program())
        .chain(c.get_args())
        .map(|s| s.to_string_lossy().into_owned())
        .collect()
}

#[cfg(unix)]
#[test]
fn only_a_demoted_child_that_can_run_is_spawned_under_utility_qos() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = crate::mktemp_dir("atv-qos").expect("mktemp");
    let fake = tmp.join("taskpolicy");
    std::fs::write(&fake, "#!/bin/sh\nexit 0\n").expect("write");
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let fake_s = fake.to_string_lossy().into_owned();
    let env = env_in(&tmp);
    let tool = Cmd::new("/bin/sh").args(["-c", "exit 0"]);
    let desk = |idle: Option<Duration>| {
        let host: &'static Idle = Box::leak(Box::new(Idle(idle)));
        ExecEnv { host, ..env }
    };
    let wrapped = [fake_s.as_str(), "-c", "utility", "/bin/sh", "-c", "exit 0"];
    for (idle, demoted, why) in [
        (Some(Duration::from_secs(5)), true, "a person typed 5 s ago"),
        (
            Some(AWAY_AFTER - Duration::from_secs(1)),
            true,
            "just inside the bound",
        ),
        (
            None,
            true,
            "an input clock that cannot be read keeps the demotion",
        ),
        (Some(AWAY_AFTER), false, "nobody at the desk for the bound"),
        (
            Some(Duration::from_secs(10 * 3600)),
            false,
            "nobody for ten hours",
        ),
    ] {
        let argv = spawned_argv(&spawn_command(&tool.clone().demoted(), desk(idle), &fake));
        if demoted {
            assert_eq!(argv, wrapped, "{why}");
        } else {
            assert_eq!(argv, ["/bin/sh", "-c", "exit 0"], "{why}");
        }
    }

    assert_eq!(
        spawned_argv(&spawn_command(&tool, env, &fake)),
        ["/bin/sh", "-c", "exit 0"],
        "a child nobody demoted keeps the inherited tier"
    );
    assert_eq!(
        spawned_argv(&spawn_command(&tool.clone().demoted(), env, &fake)),
        [fake_s.as_str(), "-c", "utility", "/bin/sh", "-c", "exit 0"]
    );
    assert_eq!(
        spawned_argv(&spawn_command(
            &tool.clone().demoted(),
            env,
            &tmp.join("absent")
        ))[0],
        "/bin/sh",
        "no taskpolicy on this machine: the tool alone"
    );
    let missing = Cmd::new(tmp.join("no-such-tool")).demoted();
    assert_eq!(
        spawned_argv(&spawn_command(&missing, env, &fake))[0],
        missing.program.to_string_lossy(),
        "a missing tool is spawned bare, so it still fails to SPAWN"
    );
    assert_eq!(
        spawned_argv(&spawn_command(&Cmd::new("sh").demoted(), env, &fake))[0],
        fake_s,
        "a bare name resolves on the child's PATH"
    );
    let hidden = Cmd::new("sh").env("PATH", tmp.as_os_str()).demoted();
    assert_eq!(
        spawned_argv(&spawn_command(&hidden, env, &fake))[0],
        "sh",
        "the child's own PATH override is the one searched"
    );
    let loader = tool
        .clone()
        .env("DYLD_FALLBACK_LIBRARY_PATH", "/tc/lib:/usr/lib")
        .env("RUSTC", "/tc/bin/rustc")
        .demoted();
    let argv = spawned_argv(&spawn_command(&loader, env, &fake));
    if crate::is_executable_file(Path::new(ENV)) {
        assert_eq!(
            argv,
            [
                fake_s.as_str(),
                "-c",
                "utility",
                ENV,
                "DYLD_FALLBACK_LIBRARY_PATH=/tc/lib:/usr/lib",
                "/bin/sh",
                "-c",
                "exit 0"
            ]
        );
    }
    assert_eq!(
        spawned_argv(&spawn_command(
            &loader.clone().env("X", "1"),
            env,
            &tmp.join("absent")
        )),
        ["/bin/sh", "-c", "exit 0"],
        "no taskpolicy: the variables stay the Command's own"
    );
    std::fs::remove_dir_all(&tmp).ok();
}
