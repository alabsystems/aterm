// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_uds::spawnfd::{Child, Stdio, command};
use std::io::{Read as _, Write as _};

const OWNER: &str = "keeper_restore_live_headless::job_cleanup::owner_actor";
const ROOT: &str = "ATERM_KEEPER_JOB_OWNER_ROOT";
const CLEANUP: &str = "keeper_restore_live_headless::job_cleanup::cleanup_actor";
const LABEL: &str = "ATERM_KEEPER_JOB_CLEANUP_LABEL";

/// Remove the registered test job when its owner's pipe closes, including after SIGKILL.
pub(super) struct Job {
    child: Child,
    label: String,
}

impl Job {
    pub(super) fn new(label: &str) -> Self {
        assert!(label.starts_with(&format!(
            "{}.{}.",
            aterm_keeper::job::TEST_LABEL_PREFIX,
            std::process::id()
        )));
        let mut cmd = command(std::env::current_exe().unwrap());
        cmd.args(["--exact", CLEANUP, "--nocapture"])
            .env(LABEL, label)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .setsid(true);
        let mut job = Self {
            child: cmd.spawn().unwrap(),
            label: label.to_string(),
        };
        aterm_uds::deadline::prints(&mut job.child, "keeper cleanup ready", HANG);
        job
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        drop(self.child.stdin.take());
        let ended = aterm_spec::wait::within(HANG, GAP, || self.child.try_wait().ok().flatten());
        if !ended.is_some_and(|status| status.success()) {
            let removed = remove_job(&self.label);
            aterm_uds::deadline::kill_within(&mut self.child, HANG);
            eprintln!("keeper cleanup helper failed; direct removal: {removed:?}");
        }
    }
}

/// `launchctl remove` acknowledges a request before launchd finishes stopping the job.
/// A successful cleanup receipt requires the registered job to have disappeared.
fn remove_job(label: &str) -> std::io::Result<()> {
    let request_error = aterm_keeper::job::remove(label).err();
    aterm_spec::wait::within(HANG, GAP, || match aterm_keeper::job::state(label) {
        Ok(aterm_keeper::job::JobState::Absent) => Some(Ok(())),
        Ok(aterm_keeper::job::JobState::Loaded { .. }) => None,
        Err(error) => Some(Err(error)),
    })
    .unwrap_or_else(|| {
        Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            format!("test keeper {label} remained registered after removal: {request_error:?}"),
        ))
    })
}

#[test]
fn cleanup_actor() {
    let Ok(label) = std::env::var(LABEL) else {
        return;
    };
    assert!(label.starts_with(&format!("{}.", aterm_keeper::job::TEST_LABEL_PREFIX)));
    println!("keeper cleanup ready");
    std::io::stdout().flush().unwrap();
    std::io::copy(&mut std::io::stdin(), &mut std::io::sink()).unwrap();
    remove_job(&label).unwrap();
    // Its owner may have died: do not write libtest's result into the closed proof pipe.
    std::process::exit(0);
}

#[test]
fn owner_actor() {
    let Some(root) = std::env::var_os(ROOT) else {
        return;
    };
    let root = PathBuf::from(root);
    let label = job_label("interrupted");
    let _job = Job::new(&label);
    aterm_keeper::job::submit(&label, "/bin/sleep", &["600"], &root.join("job.log")).unwrap();
    println!("job owner ready");
    std::io::stdout().flush().unwrap();
    let _ = std::io::stdin().read(&mut [0]);
}

struct Owner {
    child: Child,
    label: String,
}

impl Drop for Owner {
    fn drop(&mut self) {
        aterm_uds::deadline::kill_within(&mut self.child, HANG);
        let _ = aterm_keeper::job::remove(&self.label);
    }
}

#[test]
fn a_killed_test_owner_leaves_no_launchd_job() {
    killed_owner_leaves_no_job(false);
}

#[test]
fn a_killed_test_process_group_leaves_no_launchd_job() {
    killed_owner_leaves_no_job(true);
}

fn killed_owner_leaves_no_job(whole_group: bool) {
    let world = aterm_tempfile::tempdir().unwrap();
    let mut cmd = command(std::env::current_exe().unwrap());
    cmd.args(["--exact", OWNER, "--nocapture"])
        .env(ROOT, world.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .process_group(0);
    let child = cmd.spawn().unwrap();
    let label = format!(
        "{}.{}.interrupted",
        aterm_keeper::job::TEST_LABEL_PREFIX,
        child.id()
    );
    let mut owner = Owner { child, label };
    aterm_uds::deadline::prints(&mut owner.child, "job owner ready", HANG);
    assert!(matches!(
        aterm_keeper::job::state(&owner.label).unwrap(),
        aterm_keeper::job::JobState::Loaded { .. }
    ));
    if whole_group {
        let group = i32::try_from(owner.child.id()).unwrap();
        assert!(group > 1);
        // SAFETY: the ready, unreaped child leads the new group requested at spawn. No
        // unrelated session or installed keeper is in that private test group.
        assert_eq!(unsafe { libc::kill(-group, libc::SIGKILL) }, 0);
    } else {
        owner.child.kill().unwrap();
    }
    let ended = aterm_spec::wait::within(HANG, GAP, || owner.child.try_wait().unwrap())
        .expect("the killed test owner must exit");
    assert!(!ended.success());
    assert!(
        aterm_spec::wait::within(HANG, GAP, || {
            (aterm_keeper::job::state(&owner.label).unwrap() == aterm_keeper::job::JobState::Absent)
                .then_some(())
        })
        .is_some(),
        "the test's KeepAlive job outlived its killed owner"
    );
}

#[test]
fn normal_guard_drop_removes_a_registered_test_job() {
    let world = aterm_tempfile::tempdir().unwrap();
    let label = job_label("normal-drop");
    let job = Job::new(&label);
    aterm_keeper::job::submit(
        &label,
        "/bin/sleep",
        &["600"],
        &world.path().join("job.log"),
    )
    .unwrap();
    assert!(matches!(
        aterm_keeper::job::state(&label).unwrap(),
        aterm_keeper::job::JobState::Loaded { .. }
    ));
    drop(job);
    assert_eq!(
        aterm_keeper::job::state(&label).unwrap(),
        aterm_keeper::job::JobState::Absent
    );
}

#[test]
fn a_failed_cleanup_helper_still_removes_the_job_on_normal_drop() {
    let world = aterm_tempfile::tempdir().unwrap();
    let label = job_label("failed-helper");
    let mut job = Job::new(&label);
    aterm_keeper::job::submit(
        &label,
        "/bin/sleep",
        &["600"],
        &world.path().join("job.log"),
    )
    .unwrap();
    job.child.kill().unwrap();
    aterm_spec::wait::within(HANG, GAP, || job.child.try_wait().unwrap())
        .expect("the killed cleanup helper must exit");
    drop(job);
    assert_eq!(
        aterm_keeper::job::state(&label).unwrap(),
        aterm_keeper::job::JobState::Absent
    );
}
