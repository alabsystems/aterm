// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

/// A launch directory whose owner record names `owner` and whose acknowledgement is `ack`.
fn launch(owner: &Identity, ack: &Ack) -> (aterm_tempfile::TempDir, PathBuf) {
    let tmp = aterm_tempfile::Builder::new()
        .prefix("guardian-look-")
        .tempdir()
        .expect("private temp");
    let dir = tmp
        .path()
        .canonicalize()
        .expect("canonical launch")
        .join("launch");
    private_file::create_private_dir(&dir).expect("private launch");
    write(&dir, "owner.json", &owner.token()).expect("owner record");
    write(&dir, "ack.json", ack).expect("acknowledgement");
    (tmp, dir)
}

fn armed() -> Ack {
    Ack {
        done: false,
        success: true,
        reason: "local update owner armed".into(),
    }
}

fn finished() -> Ack {
    Ack {
        done: true,
        success: true,
        reason: "native ownership fixture completed".into(),
    }
}

/// This process with another birth: a recorded owner that no longer runs.
fn ended_owner() -> Identity {
    let me = identity().expect("own identity");
    Identity {
        birth: process::Birth(me.birth.0.wrapping_add(1)),
        ..me
    }
}

/// The order `look` replaced, kept here as the negative control: acknowledgement first.
fn look_acknowledgement_first(dir: &Path, between: impl FnOnce()) -> Option<Result<()>> {
    let ack = fs::read::<Ack>(dir, "ack.json").unwrap().expect("armed");
    if ack.done {
        return Some(Ok(()));
    }
    between();
    let owner = fs::read::<String>(dir, "owner.json")
        .unwrap()
        .expect("owner");
    (!live(&owner)).then(|| Err(Error::refused("ended without a final acknowledgement")))
}

#[test]
fn an_owner_that_acknowledges_and_ends_between_the_reads_is_not_refused() {
    // 0.193.0's release tree (m3, 2026-11-10): the replacement owner wrote
    // `{"done":true,"success":true}` and ended between the client's two reads, and the
    // client refused it as "ended without a final acknowledgement".
    let (_tmp, dir) = launch(&ended_owner(), &armed());
    let mut was_armed = false;
    let verdict = look(&dir, &mut was_armed, || {
        write(&dir, "ack.json", &finished()).expect("final acknowledgement");
    })
    .expect("look");
    assert!(matches!(verdict, Some(Ok(()))), "{verdict:?}");

    // Negative control: the same moment, read in the replaced order, is refused.
    let (_tmp, dir) = launch(&ended_owner(), &armed());
    let old = look_acknowledgement_first(&dir, || {
        write(&dir, "ack.json", &finished()).expect("final acknowledgement");
    });
    assert!(matches!(old, Some(Err(_))), "{old:?}");
}

#[test]
fn an_owner_that_ends_unacknowledged_is_still_refused() {
    let (_tmp, dir) = launch(&ended_owner(), &armed());
    let mut was_armed = false;
    let verdict = look(&dir, &mut was_armed, || {}).expect("look");
    let Some(Err(error)) = verdict else {
        panic!("an unacknowledged end must refuse: {verdict:?}");
    };
    assert!(
        error
            .to_string()
            .contains("ended without a final acknowledgement"),
        "{error}"
    );
    assert!(was_armed);
}

#[test]
fn a_live_owner_still_working_has_no_verdict() {
    let (_tmp, dir) = launch(&identity().expect("own identity"), &armed());
    let mut was_armed = false;
    let verdict = look(&dir, &mut was_armed, || {}).expect("look");
    assert!(verdict.is_none(), "{verdict:?}");
    assert!(was_armed);
}

/// A wait for something that must happen: a hang detector, not a latency budget.
const HANG: Duration = Duration::from_secs(60);

fn report_pipe() -> (std::io::PipeReader, std::io::PipeWriter) {
    aterm_uds::pipe().expect("exec report pipe")
}

#[test]
fn a_report_closed_unwritten_reports_no_exec_error() {
    // An exec closes the write end, and so does a child that ended before its exec.
    let (report, reporter) = report_pipe();
    drop(reporter);
    let exec = exec_report(&report, HANG);
    assert!(matches!(exec, Exec::Unreported), "{exec:?}");
}

#[test]
fn a_reported_error_is_the_failed_exec() {
    use std::io::Write as _;
    let (report, mut reporter) = report_pipe();
    reporter
        .write_all(&libc::ENOENT.to_ne_bytes())
        .expect("the child's report");
    // The write end stays open, as in a child that has reported and not yet ended.
    let exec = exec_report(&report, HANG);
    let Exec::Failed(error) = exec else {
        panic!("a whole report is the exec's error: {exec:?}");
    };
    assert_eq!(error.raw_os_error(), Some(libc::ENOENT));
}

#[test]
fn a_held_exec_ends_the_read_at_its_bound_without_a_verdict() {
    // The write end stays open and silent: an exec the system has neither run nor refused.
    let (report, reporter) = report_pipe();
    let exec = exec_report(&report, Duration::from_millis(20));
    let Exec::Unknown(reason) = exec else {
        panic!("a held exec has no verdict: {exec:?}");
    };
    assert!(reason.contains("still holds its exec"), "{reason}");
    // Negative control: the same pipe, once its write end closes, has its end to read.
    drop(reporter);
    let exec = exec_report(&report, HANG);
    assert!(matches!(exec, Exec::Unreported), "{exec:?}");
}

#[test]
fn a_cut_report_is_no_verdict_rather_than_a_failure() {
    use std::io::Write as _;
    let (report, mut reporter) = report_pipe();
    reporter.write_all(&[2, 0]).expect("half a report");
    drop(reporter);
    let exec = exec_report(&report, HANG);
    assert!(matches!(exec, Exec::Unknown(_)), "{exec:?}");
}

/// A forked child whose exec of an absent image fails: its pid, what the parent read
/// within `within`, and the scratch directory whose `log` file the child writes its line to.
fn failed_exec(within: Duration) -> (libc::pid_t, Exec, aterm_tempfile::TempDir) {
    use std::os::fd::AsRawFd as _;
    use std::os::unix::ffi::OsStrExt as _;
    let tmp = aterm_tempfile::Builder::new()
        .prefix("guardian-exec-")
        .tempdir()
        .expect("private temp");
    let program = std::ffi::CString::new(tmp.path().join("no-such-image").as_os_str().as_bytes())
        .expect("a path without NUL");
    let argv = [program.as_ptr(), std::ptr::null()];
    let envp = [std::ptr::null()];
    let log = std::fs::File::create(tmp.path().join("log")).expect("the child's log");
    // SAFETY: `program` is a C string, both arrays end in a null pointer and outlive the
    // call, and `log` is open. The forked child of this threaded test only execs, writes
    // and ends.
    let (pid, exec) =
        unsafe { exec_detached(&program, &argv, &envp, log.as_raw_fd(), within) }.expect("fork");
    (pid, exec, tmp)
}

/// Reap `pid`, our own child that ends by itself, and return its exit code.
fn exit_code(pid: libc::pid_t) -> i32 {
    let by = Instant::now() + HANG;
    let mut status = 0;
    // SAFETY: our own unreaped child; `status` is a live local.
    while unsafe { libc::waitpid(pid, &raw mut status, libc::WNOHANG) } != pid {
        assert!(Instant::now() < by, "the failed child never ended");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(libc::WIFEXITED(status), "{status}");
    libc::WEXITSTATUS(status)
}

#[test]
fn a_child_whose_exec_fails_logs_it_reports_the_error_and_ends() {
    let (pid, exec, tmp) = failed_exec(HANG);
    let Exec::Failed(error) = exec else {
        panic!("an absent image is a reported failure: {exec:?}");
    };
    assert_eq!(error.raw_os_error(), Some(libc::ENOENT));
    assert_eq!(exit_code(pid), 127);
    assert_eq!(std::fs::read(tmp.path().join("log")).unwrap(), EXEC_FAILED);
}

#[test]
fn a_failed_exec_is_logged_whether_or_not_its_report_is_read() {
    // A zero bound stops the parent at its first look, before the child has reported unless
    // the child won that race. The child keeps the pipe's read end, so its report is accepted
    // with nobody left to read it: the log line is what tells either way.
    let (pid, _read_or_not, tmp) = failed_exec(Duration::ZERO);
    assert_eq!(exit_code(pid), 127);
    assert_eq!(std::fs::read(tmp.path().join("log")).unwrap(), EXEC_FAILED);
}
