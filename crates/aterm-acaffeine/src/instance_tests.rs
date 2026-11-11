// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_second_hold_in_this_process_is_refused_until_the_first_lets_go() {
    let dir = crate::test_scratch_dir("instance");
    let path = dir.path().join("state").join("acaffeine.lock");
    let first = Instance::try_hold(&path).expect("hold").expect("free");
    assert!(Instance::try_hold(&path).expect("hold").is_none());
    drop(first);
    let again = Instance::try_hold(&path).expect("hold");
    assert!(again.is_some(), "the lock is free after the drop");
}

#[test]
#[cfg(unix)]
fn the_running_pid_is_the_holders_while_it_holds() {
    let dir = crate::test_scratch_dir("instance");
    let path = dir.path().join("acaffeine.lock");
    assert_eq!(running_pid(&path), None, "no file");
    let held = Instance::try_hold(&path).expect("hold").expect("free");
    assert_eq!(running_pid(&path), Some(std::process::id()));
    assert_eq!(
        std::fs::read_to_string(&path).expect("read"),
        format!("{}\n", std::process::id())
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(&path).expect("meta").permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    drop(held);
    assert_eq!(running_pid(&path), None, "a lock nobody holds");
    assert_eq!(
        std::fs::read_to_string(&path).expect("read"),
        "",
        "the drop leaves no pid behind"
    );
}

#[test]
#[cfg(unix)]
fn a_release_is_awaited_only_of_the_pids_named() {
    let dir = crate::test_scratch_dir("instance");
    let path = dir.path().join("acaffeine.lock");
    let me = std::process::id();
    let hang = std::time::Duration::from_secs(60);
    assert_eq!(await_released(&path, &[me], hang), Ok(()), "no file");
    let held = Instance::try_hold(&path).expect("hold").expect("free");
    assert_eq!(
        await_released(&path, &[me], std::time::Duration::ZERO),
        Err(me),
        "still held"
    );
    assert_eq!(
        await_released(&path, &[me.wrapping_add(1)], hang),
        Ok(()),
        "another pid's hold is not awaited"
    );
    let letting_go = std::thread::spawn(move || drop(held));
    assert_eq!(await_released(&path, &[me], hang), Ok(()));
    letting_go.join().expect("join");
}

#[test]
#[cfg(unix)]
fn a_requested_quit_waits_for_its_native_holder_to_release() {
    use std::cell::RefCell;
    use std::time::{Duration, Instant};
    let dir = crate::test_scratch_dir("quit-release");
    let path = dir.path().join("acaffeine.lock");
    let me = std::process::id();
    for age in [11, 61, 0] {
        let writer = RefCell::new(Some(Instance::try_hold(&path).unwrap().unwrap()));
        assert_eq!(running_pid(&path), Some(me));
        let mut released = false;
        let got = await_released_started(
            &path,
            &[me],
            QUIT_HANG,
            Instant::now() - Duration::from_secs(age),
            || {
                assert_eq!(running_pid(&path), Some(me));
                assert!(Instance::try_hold(&path).unwrap().is_none());
                drop(writer.borrow_mut().take());
                released = true;
            },
        );
        if age == 61 {
            assert_eq!(got, Err(me));
            assert!(!released);
            assert_eq!(running_pid(&path), Some(me));
            assert!(writer.borrow().is_some());
            let unrelated = await_released_started(
                &path,
                &[me.wrapping_add(1)],
                QUIT_HANG,
                Instant::now() - Duration::from_secs(61),
                || panic!("another holder is not ours to await"),
            );
            assert_eq!(unrelated, Ok(()));
            assert_eq!(running_pid(&path), Some(me));
        } else {
            assert_eq!(got, Ok(()), "the requested quit's native holder released");
            assert!(released);
            assert_eq!(running_pid(&path), None);
        }
        drop(writer.into_inner());
        assert_eq!(await_released(&path, &[me], QUIT_HANG), Ok(()));
        assert!(Instance::try_hold(&path).unwrap().is_some());
    }
}

#[test]
#[cfg(unix)]
fn a_named_job_is_written_beside_the_pid_and_read_back() {
    let dir = crate::test_scratch_dir("instance");
    let path = dir.path().join("acaffeine.lock");
    let mut held = Instance::try_hold(&path).expect("hold").expect("free");
    assert_eq!(
        running_holder(&path),
        Some(Holder {
            pid: std::process::id(),
            label: None
        })
    );
    let label = "com.aterm.aterm.acaffeine.dev.0123abcd";
    held.set_label(label).expect("label");
    assert_eq!(
        std::fs::read_to_string(&path).expect("read"),
        format!("{}\n{label}\n", std::process::id())
    );
    assert_eq!(
        running_holder(&path),
        Some(Holder {
            pid: std::process::id(),
            label: Some(label.to_string())
        })
    );
    drop(held);
    assert_eq!(running_holder(&path), None);
    assert_eq!(last_holder(&path), None, "a mug that lets go names nothing");

    let mut held = Instance::try_hold(&path).expect("hold").expect("free");
    held.set_label(label).expect("label");
    held.release_named();
    assert_eq!(running_holder(&path), None, "let go");
    assert_eq!(
        last_holder(&path).and_then(|holder| holder.label),
        Some(label.to_string())
    );
}

#[test]
#[cfg(unix)]
fn a_held_lock_with_garbage_inside_names_no_pid() {
    let dir = crate::test_scratch_dir("instance");
    let path = dir.path().join("acaffeine.lock");
    let held = Instance::try_hold(&path).expect("hold").expect("free");
    for text in ["", "x\n", "12 34\n", "-5\n", "0\n", "99999999999\n"] {
        std::fs::write(&path, text).expect("write");
        assert_eq!(running_pid(&path), None, "{text:?}");
    }
    std::fs::write(&path, "4242").expect("write");
    assert_eq!(running_pid(&path), Some(4242), "no newline is fine");
    std::fs::write(&path, format!("1\n{}\n", "a".repeat(200))).expect("write");
    assert_eq!(running_pid(&path), None, "too long");
    drop(held);
}

#[test]
#[cfg(unix)]
fn a_stale_pid_in_a_free_lock_is_not_a_running_mug() {
    let dir = crate::test_scratch_dir("instance");
    let path = dir.path().join("acaffeine.lock");
    std::fs::write(&path, "4242\n").expect("write");
    assert_eq!(running_pid(&path), None);
    std::fs::write(&path, "123456789\n").expect("write");
    let held = Instance::try_hold(&path).expect("hold").expect("free");
    assert_eq!(running_pid(&path), Some(std::process::id()));
    drop(held);
}

/// A pid left behind (a mug killed outright), read while something holds the lock (a
/// probe's instant, a `serve` that has not written its own yet), is not taken for a mug
/// when the process it now names is not one: here it is this test process, live but no
/// `acaffeine serve`.
#[test]
#[cfg(unix)]
fn a_stale_pid_under_a_held_lock_is_not_a_running_mug() {
    let dir = crate::test_scratch_dir("instance");
    let path = dir.path().join("acaffeine.lock");
    let held = Instance::try_hold(&path).expect("hold").expect("free");
    std::fs::write(
        &path,
        format!("{}\ncom.aterm.aterm.acaffeine\n", std::process::id()),
    )
    .expect("write");
    assert!(running_holder(&path).is_some(), "the file names a holder");
    assert_eq!(running_mug(&path, &dir.path().join("code")), None);
    drop(held);
}

#[test]
fn a_holder_is_its_mug_only_running_serve_from_its_jobs_place() {
    let serve =
        |exe: &str| -> Vec<OsString> { [exe, "acaffeine", "serve"].map(OsString::from).to_vec() };
    let code = Path::new("/s/acaffeine-code.noindex");
    let installed = Holder {
        pid: 7,
        label: Some(crate::launchd::INSTALLED_LABEL.to_string()),
    };
    let copy = code
        .join(crate::launchd::INSTALLED_LABEL)
        .join("aterm.app/Contents/MacOS/aterm");
    let bundle = Path::new(crate::launchd::INSTALLED_EXECUTABLE);
    for exe in [copy.as_path(), bundle] {
        let argv = serve(&exe.to_string_lossy());
        assert!(
            installed.is_run_by(exe, &argv, Some(code)),
            "{}",
            exe.display()
        );
    }
    let other = code
        .join("com.aterm.aterm.acaffeine.dev.0123abcd")
        .join("aterm.app/Contents/MacOS/aterm");
    let bare = Path::new("/w/target/debug/aterm");
    for exe in [other.as_path(), bare] {
        let argv = serve(&exe.to_string_lossy());
        assert!(
            !installed.is_run_by(exe, &argv, Some(code)),
            "{}",
            exe.display()
        );
    }
    let dev = Holder {
        pid: 7,
        label: Some(crate::launchd::label_for(bare)),
    };
    assert!(dev.is_run_by(bare, &serve("aterm"), None));
    for argv in [
        vec![OsString::from("aterm")],
        ["aterm", "acaffeine", "status"]
            .map(OsString::from)
            .to_vec(),
        ["acaffeine", "serve"].map(OsString::from).to_vec(),
        ["aterm", "serve", "acaffeine"].map(OsString::from).to_vec(),
    ] {
        assert!(!dev.is_run_by(bare, &argv, None), "{argv:?}");
    }
    let unnamed = Holder {
        pid: 7,
        label: None,
    };
    assert!(unnamed.is_run_by(&other, &serve("aterm"), Some(code)));
    assert!(!unnamed.is_run_by(&other, &[OsString::from("aterm")], Some(code)));
}

#[cfg(unix)]
#[test]
fn a_link_at_the_lock_path_is_refused_not_followed() {
    let dir = crate::test_scratch_dir("instance");
    let victim = dir.path().join("victim");
    std::fs::write(&victim, "keep me\n").expect("victim");
    let path = dir.path().join("acaffeine.lock");
    std::os::unix::fs::symlink(&victim, &path).expect("link");
    assert!(Instance::try_hold(&path).is_err());
    assert_eq!(std::fs::read_to_string(&victim).expect("read"), "keep me\n");
    assert_eq!(running_pid(&path), None);

    use std::os::fd::{FromRawFd as _, IntoRawFd as _};
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::remove_file(&path).expect("unlink");
    std::fs::set_permissions(&victim, std::fs::Permissions::from_mode(0o640)).expect("mode");
    std::fs::hard_link(&victim, &path).expect("hard link");
    assert!(Instance::try_hold(&path).is_err());
    let raw = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .expect("open")
        .into_raw_fd();
    let adopted = adopt_fd(raw, &path);
    if adopted.is_none() {
        // SAFETY: refused adoption leaves this handed-over descriptor open and owned by this test; it is closed exactly once here.
        drop(unsafe { std::fs::File::from_raw_fd(raw) });
    }
    assert!(adopted.is_none(), "a hard-linked inode is not adopted");
    assert_eq!(std::fs::read_to_string(&victim).expect("read"), "keep me\n");
    assert_eq!(
        std::fs::metadata(&victim)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777,
        0o640,
        "refused lock operations preserve the other file's mode"
    );
}

/// A FIFO planted at the lock path is opened without waiting for a writer and refused as
/// not a regular file; no pid is read from it.
#[cfg(unix)]
#[test]
fn a_fifo_at_the_lock_path_is_refused_without_waiting() {
    use std::os::unix::ffi::OsStrExt as _;
    let dir = crate::test_scratch_dir("instance");
    let path = dir.path().join("acaffeine.lock");
    let name = std::ffi::CString::new(path.as_os_str().as_bytes()).expect("no NUL");
    // SAFETY: a NUL-terminated path the call reads and does not retain.
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0, "mkfifo");
    let err = Instance::try_hold(&path).expect_err("a FIFO");
    assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{err}");
    assert_eq!(running_pid(&path), None);
}

#[test]
fn adopt_without_the_variable_is_none() {
    assert!(std::env::var_os(LOCK_FD_ENV).is_none());
    let dir = crate::test_scratch_dir("instance");
    assert!(Instance::adopt(&dir.path().join("acaffeine.lock")).is_none());
}

/// A closed copy of the hold's descriptor (what the exec does to the old image's) leaves the
/// lock held; a handed-over one is adopted as the hold, with the lock held throughout, and
/// the adopted hold's drop lets it go (`LOCK_UN` is on the shared description).
#[cfg(unix)]
#[test]
fn a_handed_over_descriptor_is_adopted_with_its_lock() {
    use std::os::fd::IntoRawFd as _;
    let dir = crate::test_scratch_dir("instance");
    let path = dir.path().join("acaffeine.lock");
    let first = Instance::try_hold(&path).expect("hold").expect("free");
    drop(first.handover().expect("handover"));
    assert!(Instance::try_hold(&path).expect("hold").is_none());
    let raw = first.handover().expect("handover").into_raw_fd();
    assert!(raw > 2, "a copy is never a stdio slot");
    let adopted = adopt_fd(raw, &path).expect("adopted");
    assert_eq!(running_pid(&path), Some(std::process::id()));
    assert!(Instance::try_hold(&path).expect("hold").is_none());
    drop(adopted);
    let again = Instance::try_hold(&path).expect("hold");
    assert!(again.is_some(), "the adopted hold's drop let it go");
    drop(again);
    drop(first);
}

#[cfg(unix)]
#[test]
fn a_descriptor_on_another_file_is_not_adopted() {
    use std::os::fd::{FromRawFd as _, IntoRawFd as _};
    let dir = crate::test_scratch_dir("instance");
    let path = dir.path().join("acaffeine.lock");
    let held = Instance::try_hold(&path).expect("hold").expect("free");
    let other = dir.path().join("other");
    std::fs::write(&other, "x").expect("other");
    let raw = std::fs::File::open(&other).expect("open").into_raw_fd();
    assert!(adopt_fd(raw, &path).is_none());
    // SAFETY: F_GETFD on a number this test still owns.
    assert!(
        unsafe { libc::fcntl(raw, libc::F_GETFD) } >= 0,
        "left as it was"
    );
    // SAFETY: `raw` is this test's own open descriptor, closed once here.
    drop(unsafe { std::fs::File::from_raw_fd(raw) });
    assert!(adopt_fd(-1, &path).is_none());
    assert!(adopt_fd(i32::MAX, &path).is_none(), "not open");
    drop(held);
}

/// A descriptor on the lock file that does not hold it, while another does, is refused. It is
/// open for writing, as a handed-over hold is, so the refusal is the lock's and not a failed
/// write of the pid.
#[cfg(unix)]
#[test]
fn a_descriptor_on_the_lock_another_holds_is_not_adopted() {
    use std::os::fd::IntoRawFd as _;
    let dir = crate::test_scratch_dir("instance");
    let path = dir.path().join("acaffeine.lock");
    let held = Instance::try_hold(&path).expect("hold").expect("free");
    let raw = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .expect("open")
        .into_raw_fd();
    assert!(adopt_fd(raw, &path).is_none());
    assert_eq!(running_pid(&path), Some(std::process::id()));
    drop(held);
}

#[test]
fn holders_parse_strictly() {
    let pid = |pid| Some(Holder { pid, label: None });
    assert_eq!(parse_holder("1\n"), pid(1));
    assert_eq!(parse_holder("4242"), pid(4242));
    let named = Some(Holder {
        pid: 9,
        label: Some("com.aterm.aterm.acaffeine".to_string()),
    });
    assert_eq!(parse_holder("9\ncom.aterm.aterm.acaffeine\n"), named);
    assert_eq!(parse_holder("9\ncom.aterm.aterm.acaffeine"), named);
    for bad in [
        "",
        "\n",
        "0",
        " 1\n",
        "1 \n",
        "1\n\n",
        "+1",
        "x",
        "4294967296",
        "9\ncom.example.other\n",
        "9\ncom.aterm.aterm.acaffeine\nextra\n",
        "\ncom.aterm.aterm.acaffeine\n",
    ] {
        assert_eq!(parse_holder(bad), None, "{bad:?}");
    }
}

#[cfg(target_os = "macos")]
#[test]
fn this_boot_reads_as_one_id() {
    let boot = boot_id().expect("kern.bootsessionuuid");
    assert_ne!(boot, 0);
    assert_eq!(boot_id(), Some(boot));
    assert_eq!(
        crate::state::parse_boot(&crate::state::boot_text(boot)),
        Some(boot)
    );
}
