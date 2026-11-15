// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

use aterm_spec::wait::{MUST_HAPPEN, until_within};

/// Set, the test binary is [`standin_front_handles_and_becomes`].
const FRONT: &str = "ATPKG_BECOMING_FRONT";
/// Set, the test binary is [`standin_reports_what_it_was_started_with`].
const REPORT: &str = "ATPKG_BECOMING_REPORT";
/// Set, the test binary is [`standin_front_goes_through_the_fronts_exec`]: `held` and `free` go
/// through the front's exec, `pass-held` and `pass-free` through a pass-through's, and a `held`
/// one holds a process-table read of its own inside its exec gate first.
#[cfg(target_os = "macos")]
const GATED_FRONT: &str = "ATPKG_BECOMING_GATED_FRONT";
/// Set to an exit code, the stand-in build says which process it is and ends with that code.
const TOLD: &str = "ATPKG_BECOMING_TOLD";
/// Set to a descriptor's number, the stand-in build says whether it was started with it open.
const HANDED: &str = "ATPKG_BECOMING_HANDED";
/// The cover marker the gated front hands its build.
#[cfg(target_os = "macos")]
const MARKER: &str = "becoming-test-marker";

/// Whether `sig`'s disposition `INHERITED` read at load is to ignore it.
fn ignored(sig: libc::c_int) -> bool {
    INHERITED.get().is_some_and(|i| {
        i.actions
            .iter()
            .any(|(s, a)| *s == sig && a.sa_sigaction == libc::SIG_IGN)
    })
}

extern "C" fn noted(_: libc::c_int) {}

/// The front, as a stand-in: catches SIGINT with a handler of its own (as an admission wait
/// does), then becomes [`standin_reports_what_it_was_started_with`].
#[test]
fn standin_front_handles_and_becomes() {
    if std::env::var_os(FRONT).is_none() {
        return;
    }
    // SAFETY: `noted` is an `extern "C" fn(c_int)` that touches nothing.
    unsafe {
        libc::signal(
            libc::SIGINT,
            noted as extern "C" fn(libc::c_int) as libc::sighandler_t,
        )
    };
    let exe = std::env::current_exe().expect("test binary");
    match become_build(&exe, &BUILD, None) {
        Stayed::Failed(err) => panic!("the exec failed: {err}"),
        #[cfg(target_os = "macos")]
        Stayed::Reading(still) => panic!("no exec was made: {still}"),
    }
}

/// The words that make the test binary [`standin_reports_what_it_was_started_with`].
const BUILD: [&str; 4] = [
    "--exact",
    "build_admit::becoming::tests::standin_reports_what_it_was_started_with",
    "--test-threads=1",
    "--nocapture",
];

/// The build, as a stand-in: says which of SIGPIPE and SIGINT it was started ignoring or, for
/// a front that says what it is ([`TOLD`]), which process it is, whose child, under which cover
/// marker and whether the descriptor [`HANDED`] names is open, then ends with the code [`TOLD`]
/// names.
#[test]
fn standin_reports_what_it_was_started_with() {
    if std::env::var_os(REPORT).is_none() {
        return;
    }
    if let Some(code) = std::env::var(TOLD).ok().and_then(|c| c.parse::<i32>().ok()) {
        // SAFETY: `getppid` takes nothing and cannot fail.
        let parent = unsafe { libc::getppid() };
        let handed = match std::env::var(HANDED).ok().and_then(|fd| fd.parse().ok()) {
            None => "none",
            // SAFETY: `F_GETFD` reads one descriptor's flags; a closed number answers -1.
            Some(fd) if unsafe { libc::fcntl(fd, libc::F_GETFD) } == -1 => "closed",
            Some(_) => "open",
        };
        println!(
            "BUILD pid={} parent={parent} handed={handed} cover={}",
            std::process::id(),
            std::env::var(aterm_capacity::COVER_ENV).unwrap_or_default()
        );
        std::process::exit(code);
    }
    println!(
        "REPORT pipe={} int={}",
        if ignored(libc::SIGPIPE) {
            "ignored"
        } else {
            "default"
        },
        if ignored(libc::SIGINT) {
            "ignored"
        } else {
            "default"
        }
    );
}

/// The report of a front started by `sh` with `trap` set (empty for none), from this process
/// with SIGINT at its default action, set here, not the runner's (a `&` job's is ignored): the
/// shell, and through it the front, inherit it. (SIGPIPE, which this process's runtime
/// ignores, the spawn puts back to its default in the shell.)
fn report(trap: &str) -> String {
    let exe = std::env::current_exe().expect("test binary");
    let start = crate::platform::Starting::at_defaults(&[libc::SIGINT]);
    let mut child = start.spawning(|| {
        aterm_uds::spawnfd::command("/bin/sh")
            .args([
                "-c",
                &format!(
                    "{trap} exec \"$0\" --exact \
                     build_admit::becoming::tests::standin_front_handles_and_becomes \
                     --test-threads=1 --nocapture"
                ),
            ])
            .arg(&exe)
            .env(FRONT, "1")
            .env(REPORT, "1")
            .stdin(aterm_uds::spawnfd::Stdio::null())
            .stdout(aterm_uds::spawnfd::Stdio::piped())
            .stderr(aterm_uds::spawnfd::Stdio::null())
            .spawn()
            .expect("the front")
    });
    drop(start);
    let status = until_within(MUST_HAPPEN, "the stand-ins' end", || {
        child.try_wait().ok().flatten()
    });
    let mut out = String::new();
    if let Some(mut o) = child.stdout.take() {
        let _ = std::io::Read::read_to_string(&mut o, &mut out);
    }
    assert!(status.success(), "{status} {out}");
    out.lines()
        .find_map(|l| l.split_once("REPORT ").map(|(_, said)| said))
        .unwrap_or("no report")
        .to_owned()
}

/// A BUILD STARTS WITH THE SIGNAL STATE ITS FRONT WAS STARTED WITH: a front started ignoring
/// SIGPIPE and SIGINT, which caught SIGINT itself, hands its build both ignored; one started
/// at the defaults hands both at the defaults, not the SIGPIPE its runtime ignored. Both start
/// from a state the test sets ([`report`]), whatever the runner was started with.
/// NEGATIVE CONTROL (by hand: `become_build` with `put_back` skipped, std's exec): the first
/// build is started with both at their defaults.
#[test]
fn a_build_starts_with_the_signal_state_its_front_was_started_with() {
    assert_eq!(report("trap '' PIPE INT;"), "pipe=ignored int=ignored");
    assert_eq!(report(""), "pipe=default int=default");
}

/// The front, as a stand-in that goes through the front's own exec ([`super::super::exec_marked`])
/// or, in a `pass-` mode, through a pass-through's ([`crate::platform::exec_or_run`], with a
/// descriptor of its own that an exec hands on): says which process it is, then becomes
/// [`standin_reports_what_it_was_started_with`] or, with a read of its own held in its exec
/// gate, runs it and ends as it ended. The held read stands for the front's `lane-kproc_table`
/// thread, in the kernel behind another process's exec: a thread parked inside the gate, which
/// an exec made over it would simply end.
#[cfg(target_os = "macos")]
#[test]
fn standin_front_goes_through_the_fronts_exec() {
    let Some(mode) = std::env::var(GATED_FRONT).ok() else {
        return;
    };
    if mode.ends_with("held") {
        let (entered, in_kernel) = std::sync::mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("lane-kproc_table".into())
            .spawn(move || {
                let _in_kernel = aterm_sysprobe::exec_gate::reading();
                let _ = entered.send(());
                loop {
                    std::thread::park();
                }
            })
            .expect("the reader's thread");
        in_kernel
            .recv_timeout(MUST_HAPPEN)
            .expect("the read is inside the gate");
    }
    println!("FRONT pid={}", std::process::id());
    let exe = std::env::current_exe().expect("test binary");
    if mode.starts_with("pass-") {
        // SAFETY: `dup` of this process's standard output: a descriptor with no close-on-exec
        // mark, as a caller's jobserver pipe is.
        let handed = unsafe { libc::dup(libc::STDOUT_FILENO) };
        assert!(handed > libc::STDERR_FILENO, "a descriptor to hand on");
        let mut command = std::process::Command::new(&exe);
        command.args(BUILD).env(HANDED, handed.to_string());
        let stayed = crate::platform::exec_or_run(&mut command);
        panic!("neither the exec nor the child's run ended this process: {stayed}");
    }
    let end = super::super::exec_marked(&exe, &BUILD, Some(MARKER));
    println!("FRONT ended {end:?}");
}

/// What a gated front and its build said, and how the front ended.
#[cfg(target_os = "macos")]
struct Gated {
    /// The front's pid, as it said it.
    front: u32,
    /// The build's pid and its parent's, as it said them.
    build: (u32, u32),
    /// The cover marker the build was started with.
    cover: String,
    /// Whether the build was started with the descriptor its front named: `open`, `closed`, or
    /// `none` where the front named none.
    handed: String,
    /// What the front said after its build ended; `None` where it became the build.
    ended: Option<String>,
    /// The front's exit code.
    code: Option<i32>,
    /// What the front wrote to its standard error.
    said: String,
}

/// `pipe` read to its end, on a thread of its own.
#[cfg(target_os = "macos")]
fn draining(pipe: Option<impl std::io::Read + Send + 'static>) -> std::thread::JoinHandle<String> {
    std::thread::spawn(move || {
        let mut text = String::new();
        if let Some(mut pipe) = pipe {
            let _ = pipe.read_to_string(&mut text);
        }
        text
    })
}

/// Run [`standin_front_goes_through_the_fronts_exec`] in `mode` with a build that ends with 7.
/// A front that has not ended within the hang detector is killed and the test fails by name.
#[cfg(target_os = "macos")]
fn gated(mode: &str) -> Gated {
    let exe = std::env::current_exe().expect("test binary");
    let mut child = crate::platform::spawn_outside_a_stand_aside(|| {
        aterm_uds::spawnfd::command(&exe)
            .args([
                "--exact",
                "build_admit::becoming::tests::standin_front_goes_through_the_fronts_exec",
                "--test-threads=1",
                "--nocapture",
            ])
            .env(GATED_FRONT, mode)
            .env(REPORT, "1")
            .env(TOLD, "7")
            .env_remove(aterm_capacity::COVER_ENV)
            .stdin(aterm_uds::spawnfd::Stdio::null())
            .stdout(aterm_uds::spawnfd::Stdio::piped())
            .stderr(aterm_uds::spawnfd::Stdio::piped())
            .spawn()
            .expect("the front")
    });
    let (out, said) = (draining(child.stdout.take()), draining(child.stderr.take()));
    let ended = aterm_spec::wait::within(MUST_HAPPEN, std::time::Duration::from_millis(10), || {
        child.try_wait().ok().flatten()
    });
    let Some(status) = ended else {
        aterm_uds::deadline::kill_within(&mut child, MUST_HAPPEN);
        panic!("hang detector: the gated front ({mode}) had not ended after {MUST_HAPPEN:?}");
    };
    let (out, said) = (
        out.join().unwrap_or_default(),
        said.join().unwrap_or_default(),
    );
    let word = |line: &str, key: &str| {
        line.split_whitespace()
            .find_map(|w| w.strip_prefix(key).map(str::to_owned))
    };
    let line = |starts: &str| {
        out.lines()
            .find_map(|l| l.find(starts).map(|at| l[at..].to_owned()))
    };
    let pid = |line: &Option<String>, key: &str| {
        line.as_deref()
            .and_then(|l| word(l, key))
            .and_then(|p| p.parse::<u32>().ok())
            .unwrap_or_else(|| panic!("no {key} in {out:?} ({said:?})"))
    };
    let (front, build) = (line("FRONT pid="), line("BUILD pid="));
    let said_of = |key: &str| {
        build
            .as_deref()
            .and_then(|l| word(l, key))
            .unwrap_or_default()
    };
    Gated {
        front: pid(&front, "pid="),
        build: (pid(&build, "pid="), pid(&build, "parent=")),
        cover: said_of("cover="),
        handed: said_of("handed="),
        ended: line("FRONT ended "),
        code: status.code(),
        said,
    }
}

/// A FRONT WITH A PROCESS-TABLE READ OF ITS OWN STILL IN THE KERNEL DOES NOT EXEC: it runs its
/// build as its child, under the cover marker, says why, and ends as the build ended. With no
/// such read it becomes the build: one process, its caller's child, ending as the build ends.
/// An exec made over the read is what left seven fronts in exec for good (m72, 2026-11-12):
/// each waited on its own table reader's thread (`kproc-table` in that build), which waited on
/// another front's exec.
/// NEGATIVE CONTROL (by hand: `become_build` without its gate's `close`): the first front
/// becomes its build, so the build's pid is the front's and no `FRONT ended` is said.
#[cfg(target_os = "macos")]
#[test]
fn a_front_with_a_table_read_in_the_kernel_runs_its_build_as_its_child() {
    let held = gated("held");
    assert_eq!(held.build.1, held.front, "the build is the front's child");
    assert_ne!(
        held.build.0, held.front,
        "the front did not become the build"
    );
    assert_eq!(held.cover, MARKER);
    assert_eq!(
        held.ended.as_deref(),
        Some(format!("FRONT ended {:?}", std::process::ExitCode::from(7)).as_str()),
        "the front ends as its build ended: {}",
        held.said
    );
    assert!(
        held.said.contains("still in the kernel"),
        "the front says why it did not exec: {}",
        held.said
    );
    let free = gated("free");
    assert_eq!(free.build.0, free.front, "the front became the build");
    assert_eq!(free.build.1, std::process::id(), "its caller's child");
    assert_eq!(free.cover, MARKER);
    assert_eq!((free.ended, free.code), (None, Some(7)));
}

/// A PASS-THROUGH WITH A PROCESS-TABLE READ OF ITS OWN STILL IN THE KERNEL DOES NOT EXEC EITHER
/// ([`crate::platform::exec_or_run`], a tool's shim and `aterm pkg run`): it runs its program as
/// its child, with the descriptors an exec would have handed it (a caller's jobserver pipe),
/// says why, and leaves as the program left. With no such read it becomes the program.
/// NEGATIVE CONTROL (by hand: `run_in_place_of_exec` without `inherit_everything`): the child
/// is started with the descriptor closed.
#[cfg(target_os = "macos")]
#[test]
fn a_pass_through_with_a_table_read_in_the_kernel_runs_its_program_as_its_child() {
    let held = gated("pass-held");
    assert_eq!(held.build.1, held.front, "the program is its child");
    assert_ne!(held.build.0, held.front, "it did not become the program");
    assert_eq!(held.handed, "open", "an exec would have handed it on");
    assert_eq!(held.code, Some(7), "it leaves as its program left");
    assert!(
        held.said.contains("still in the kernel") && held.said.contains("runs as a child"),
        "it says why it did not exec: {}",
        held.said
    );
    let free = gated("pass-free");
    assert_eq!(free.build.0, free.front, "it became the program");
    assert_eq!(free.build.1, std::process::id(), "its caller's child");
    assert_eq!((free.handed.as_str(), free.code), ("open", Some(7)));
}
