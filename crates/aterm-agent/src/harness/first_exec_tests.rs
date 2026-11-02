// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use std::io::Write as _;
use std::time::{Duration, Instant, SystemTime};

const HANG: Duration = aterm_spec::wait::MUST_HAPPEN;
const FIXTURE: &str = "harness::first_exec::tests::program_inspection_process";
const FILE_ENV: &str = "ATERM_FIRST_EXEC_INSPECT_FILE";
const READY_ENV: &str = "ATERM_FIRST_EXEC_INSPECT_READY";

fn until(mut done: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now().checked_add(HANG).expect("hang detector");
    loop {
        if done() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

struct Child(aterm_uds::spawnfd::Child);

impl Child {
    fn retire(&mut self) -> bool {
        if until(|| self.0.try_wait().is_ok_and(|status| status.is_some())) {
            return self.0.wait().is_ok_and(|status| status.success());
        }
        let _ = self.0.kill();
        let _ = self.0.wait();
        false
    }
}

impl Drop for Child {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

// Reexecutes the already-running Rust test image in place; no copy or Apple helper is launched.
#[test]
fn program_inspection_process() {
    let Some(file) = std::env::var_os(FILE_ENV) else {
        return;
    };
    let ready = PathBuf::from(std::env::var_os(READY_ENV).expect("fixture ready path"));
    std::fs::write(&ready, b"entered").expect("ready");
    let _ = program_file(Path::new(&file));
    std::fs::write(ready.with_extension("done"), b"returned").expect("done");
}

#[test]
fn program_inspection_keeps_native_and_twin_aliases_and_never_waits_on_a_fifo() {
    use std::os::unix::ffi::OsStrExt as _;
    use std::os::unix::fs::OpenOptionsExt as _;

    let scratch = aterm_tempfile::tempdir().expect("scratch");
    let native = scratch.path().join("native");
    std::fs::write(&native, b"ordinary inert program data").unwrap();
    let alias = scratch.path().join("alias");
    std::os::unix::fs::symlink(&native, &alias).unwrap();
    assert_eq!(
        program_file(&alias),
        std::fs::canonicalize(&native).unwrap()
    );
    let twin = scratch.path().join("twin");
    std::fs::write(
        &twin,
        format!("#!/bin/sh\nexec '{}' \"$@\"\n", alias.display()),
    )
    .unwrap();
    assert_eq!(program_file(&twin), std::fs::canonicalize(&native).unwrap());

    let fifo = scratch.path().join("candidate");
    let name = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
    // SAFETY: a NUL-terminated owned scratch path, no existing file and no pointer retained.
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    let ready = scratch.path().join("inspection");
    let mut child = Child(
        aterm_uds::spawnfd::command(std::env::current_exe().unwrap())
            .args(["--exact", FIXTURE, "--nocapture"])
            .env(FILE_ENV, &fifo)
            .env(READY_ENV, &ready)
            .stdin(aterm_uds::spawnfd::Stdio::null())
            .stdout(aterm_uds::spawnfd::Stdio::null())
            .stderr(aterm_uds::spawnfd::Stdio::null())
            .spawn()
            .expect("same Rust image"),
    );
    let entered = until(|| ready.is_file());
    let returned_without_writer = entered && until(|| ready.with_extension("done").is_file());
    if entered && !returned_without_writer {
        // Release OLD's blocked read before judging it. This data is never executed.
        let mut writer = None;
        let opened = until(|| {
            match std::fs::OpenOptions::new()
                .write(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(&fifo)
            {
                Ok(file) => {
                    writer = Some(file);
                    true
                }
                Err(_) => child.0.try_wait().is_ok_and(|status| status.is_some()),
            }
        });
        if opened && let Some(mut writer) = writer {
            let _ = writer.write_all(b"ordinary data\n");
        }
    }
    let retired = child.retire();
    assert!(
        entered && retired,
        "the fixture entered and retired before judgment"
    );
    assert!(
        returned_without_writer,
        "program inspection waited for a FIFO writer before its deadline"
    );
}

#[test]
fn unknown_or_incomplete_first_exec_inventory_never_admits_removal() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let tree = scratch.path().join("version");
    std::fs::write(&tree, b"owned data").unwrap();
    let holds = scratch.path().join("holds");
    assert!(
        !held(&holds, &tree, SystemTime::now()),
        "ordinary missing inventory is empty"
    );
    std::fs::create_dir(&holds).unwrap();
    assert!(
        !held(&holds, &tree, SystemTime::now()),
        "ordinary readable empty inventory"
    );
    std::fs::remove_dir(&holds).unwrap();
    std::fs::write(&holds, b"not an inventory directory").unwrap();
    let refused_inventory = held(&holds, &tree, SystemTime::now());
    let incomplete = held_among(
        Ok(vec![Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "unread entry",
        ))]
        .into_iter()),
        &tree,
        SystemTime::now(),
    );
    assert!(
        refused_inventory && incomplete,
        "unknown root={refused_inventory}, incomplete entries={incomplete}"
    );
}

#[test]
fn native_removal_and_registration_are_one_ordered_decision() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let holds = scratch.path().join("state/holds");
    let tree = scratch.path().join("version");
    let exe = tree.join("program");
    std::fs::create_dir(&tree).unwrap();
    std::fs::write(&exe, b"owned data, never executed").unwrap();
    let standing = hold(&holds, &exe).unwrap();
    assert!(remove_when_unheld(&holds, &tree, SystemTime::now(), || Ok(())).is_err());
    standing.ran();
    assert!(remove_when_unheld(&holds, &tree, SystemTime::now(), || Ok(())).is_ok());

    let registration = std::cell::RefCell::new(None);
    let removed = remove_when_unheld(&holds, &tree, SystemTime::now(), || {
        *registration.borrow_mut() = Some(hold(&holds, &exe));
        std::fs::remove_dir_all(&tree)
    });
    let prevented = match registration.into_inner().expect("callback reached") {
        Err(error) => error.kind() == std::io::ErrorKind::WouldBlock,
        Ok(hold) => {
            hold.ran();
            false
        }
    };
    assert!(removed.is_ok(), "ordinary unheld version removed");
    assert!(
        prevented,
        "a starter registered between the inventory check and deletion"
    );
}

#[test]
fn natural_probe_exit_and_failed_spawn_release_their_own_holds() {
    use super::super::upgrade_drive::first_exec_probe_for_test;

    let scratch = aterm_tempfile::tempdir().unwrap();
    let holds = scratch.path().join("holds");
    let image = std::env::current_exe().unwrap();
    assert_eq!(first_exec_probe_for_test(&image, HANG, &holds), "starts");
    assert!(!held(&holds, &image, SystemTime::now()));
    let missing = scratch.path().join("absent");
    assert_eq!(first_exec_probe_for_test(&missing, HANG, &holds), "no-exec");
    assert!(!held(&holds, scratch.path(), SystemTime::now()));
}
