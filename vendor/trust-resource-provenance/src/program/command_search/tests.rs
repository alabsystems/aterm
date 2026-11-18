//! Actual physical-input custody controls. Nothing here selects a program,
//! changes the ordinary selector, funds a native child or issues a Seed/SDK.

use super::*;
use std::os::unix::{
    ffi::OsStringExt,
    fs::{PermissionsExt, symlink},
};
use std::time::Duration;

fn end() -> Instant {
    Instant::now() + Duration::from_secs(60)
}
fn fixture() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    std::fs::create_dir(root.join("early")).unwrap();
    std::fs::create_dir(root.join("late")).unwrap();
    std::fs::write(root.join("late/otool"), b"read-only candidate DATA").unwrap();
    (tmp, root)
}
fn paths(roots: &[PathBuf]) -> OsString {
    std::env::join_paths(roots).unwrap()
}
fn capture(root: &Path, path: &OsStr) -> HeldCommandSearchInputs {
    HeldCommandSearchInputs::capture(root, path, OsStr::new("otool"), end()).unwrap()
}

#[test]
fn all_original_candidates_are_held_without_selecting_or_reading_their_contents() {
    let (_tmp, r) = fixture();
    let held = capture(&r, &paths(&[r.join("early"), r.join("late")]));
    held.check_current().unwrap();
    // Unrelated directory siblings are not listing inputs to which/access.
    std::fs::write(r.join("early/unrelated"), b"unrelated").unwrap();
    held.check_current().unwrap();
    std::fs::write(r.join("early/otool"), b"new earlier candidate").unwrap();
    assert!(held.check_current().is_err());
    std::fs::remove_file(r.join("early/otool")).unwrap();
    assert!(
        held.check_current().is_err(),
        "restored absence cannot revive failed custody"
    );
}

#[test]
fn candidate_membership_is_exact_lexical_data_not_availability_or_selection() {
    let (_tmp, r) = fixture();
    let held = capture(&r, OsStr::new("early:late"));
    held.check_candidate(Path::new("early/otool")).unwrap(); // absent, but observed
    held.check_candidate(Path::new("late/otool")).unwrap();
    assert!(held.check_candidate(Path::new("./late/otool")).is_err());
    assert!(held.check_candidate(Path::new("late/otool")).is_err());
    let held = capture(&r, OsStr::new("early:late"));
    assert!(
        held.check_candidate(&r.join("late/otool")).is_err(),
        "equal referent does not rewrite the original lexical selection"
    );
}

#[test]
fn added_physical_reads_never_broaden_original_path_selection() {
    let (_tmp, r) = fixture();
    let held = capture(&r, OsStr::new("early"));
    held.retain_inputs(&[r.join("late/otool")]).unwrap();
    held.check_candidate(Path::new("early/otool")).unwrap();
    assert!(held.check_candidate(&r.join("late/otool")).is_err());
    assert!(held.check_current().is_err());
}

#[test]
fn later_selection_link_and_negative_contents_reads_remain_original_inputs() {
    let (_tmp, r) = fixture();
    symlink("late", r.join("selection")).unwrap();
    let held = capture(&r, OsStr::new("early"));
    held.retain_inputs(&[r.join("selection"), r.join("late/Contents/Developer")])
        .unwrap();
    // The additional absence, rather than a guessed selected tool, is held.
    std::fs::create_dir(r.join("late/Contents")).unwrap();
    assert!(held.check_current().is_err());
    std::fs::remove_dir(r.join("late/Contents")).unwrap();
    assert!(held.check_current().is_err());

    let held = capture(&r, OsStr::new("early"));
    held.retain_inputs(&[r.join("selection")]).unwrap();
    std::fs::remove_file(r.join("selection")).unwrap();
    symlink("late", r.join("selection")).unwrap();
    assert!(
        held.check_current().is_err(),
        "same link text is not the same original"
    );
}

#[test]
fn failed_extension_retains_originals_quarantined_and_cannot_retry() {
    let (_tmp, r) = fixture();
    let held = capture(&r, OsStr::new("early"));
    assert!(
        held.retain_inputs(&[r.join("late/otool"), PathBuf::from("relative")])
            .is_err()
    );
    assert!(held.retain_inputs(&[r.join("late/otool")]).is_err());
    assert!(held.check_candidate(Path::new("early/otool")).is_err());

    let held = capture(&r, OsStr::new("early"));
    assert!(
        held.retain_inputs(&vec![r.join("late/otool"); MAX_ROOTS + 1])
            .is_err()
    );
    assert!(held.check_current().is_err());
}

#[test]
fn missing_directory_prefixes_and_candidates_after_a_possible_winner_remain_inputs() {
    let (_tmp, r) = fixture();
    let held = capture(&r, &paths(&[r.join("late"), r.join("missing/deeper")]));
    std::fs::create_dir(r.join("missing")).unwrap();
    assert!(held.check_current().is_err());
    let held = capture(&r, &paths(&[r.join("late"), r.join("early")]));
    std::fs::write(r.join("early/otool"), b"changed later candidate").unwrap();
    assert!(
        held.check_current().is_err(),
        "no semantic guess stops the input population early"
    );
}

#[test]
fn relative_empty_and_parent_path_components_keep_original_cwd_semantics() {
    let (_tmp, r) = fixture();
    let held = capture(&r, OsStr::new(":early/../late:./early"));
    held.check_current().unwrap();
    std::fs::write(r.join("otool"), b"new relative candidate").unwrap();
    assert!(held.check_current().is_err());
    let held = capture(&r, OsStr::new(""));
    std::fs::write(r.join("late/otool"), b"empty PATH reads no candidate").unwrap();
    held.check_current().unwrap();
}

#[test]
fn complete_literal_symlink_reach_is_held_including_absolute_relative_and_broken_targets() {
    let (_tmp, r) = fixture();
    symlink("late", r.join("alias")).unwrap();
    symlink(r.join("alias"), r.join("absolute")).unwrap();
    symlink("does-not-exist/bin", r.join("broken")).unwrap();
    let held = capture(&r, &paths(&[r.join("absolute"), r.join("broken")]));
    held.check_current().unwrap();
    std::fs::remove_file(r.join("alias")).unwrap();
    symlink("late", r.join("alias")).unwrap();
    assert!(
        held.check_current().is_err(),
        "same text at a new link inode is a changed original"
    );
    let held = capture(&r, &paths(&[r.join("broken")]));
    std::fs::create_dir(r.join("does-not-exist")).unwrap();
    assert!(held.check_current().is_err());
}

#[test]
fn selected_metadata_changes_and_same_byte_file_or_directory_replacements_refuse() {
    let (_tmp, r) = fixture();
    let held = capture(&r, &paths(&[r.join("late")]));
    let bytes = std::fs::read(r.join("late/otool")).unwrap();
    std::fs::write(r.join("replacement"), &bytes).unwrap();
    std::fs::rename(r.join("replacement"), r.join("late/otool")).unwrap();
    assert!(held.check_current().is_err());
    let held = capture(&r, &paths(&[r.join("late")]));
    std::fs::set_permissions(r.join("late/otool"), std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(
        held.check_current().is_err(),
        "execute permissions are a selected which input"
    );
    let held = capture(&r, &paths(&[r.join("early")]));
    std::fs::rename(r.join("early"), r.join("old-early")).unwrap();
    std::fs::create_dir(r.join("early")).unwrap();
    assert!(held.check_current().is_err());
}

#[cfg(target_os = "macos")]
#[test]
fn deny_search_acl_drift_on_a_parent_component_is_an_original_input_change() {
    let (_tmp, r) = fixture();
    std::fs::set_permissions(r.join("late/otool"), std::fs::Permissions::from_mode(0o700)).unwrap();
    let path = r.join("early/../late/otool");
    let raw = CString::new(path.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::access(raw.as_ptr(), libc::X_OK) }, 0);
    let held = capture(&r, &paths(&[r.join("early/../late")]));
    let mut command = aterm_uds::spawnfd::command("/bin/chmod");
    command
        .args(["+a", "everyone deny search"])
        .arg(r.join("early"));
    assert!(
        aterm_uds::deadline::output_within(&mut command, Duration::from_secs(60))
            .unwrap()
            .status
            .success()
    );
    let changed_access = unsafe { libc::access(raw.as_ptr(), libc::X_OK) };
    let refused = held.check_current().is_err();
    // Restore only this own fixture before asserting or letting TempDir retire
    // it; the denial never reaches another owner's path or an ancestor.
    let mut reset = aterm_uds::spawnfd::command("/bin/chmod");
    reset.arg("-N").arg(r.join("early"));
    assert!(
        aterm_uds::deadline::output_within(&mut reset, Duration::from_secs(60))
            .unwrap()
            .status
            .success()
    );
    assert_ne!(
        changed_access, 0,
        "actual kernel access must expose the changed search premise"
    );
    assert!(
        refused,
        "deny-only ACL drift cannot be hidden by directory identity/permission equality"
    );
    assert!(
        held.check_current().is_err(),
        "restored ACL cannot revive failed custody"
    );
}

#[test]
fn non_directories_block_suffixes_but_cannot_become_directories_unnoticed() {
    let (_tmp, r) = fixture();
    std::fs::write(r.join("not-a-directory"), b"ordinary metadata").unwrap();
    let held = capture(&r, &paths(&[r.join("not-a-directory/bin")]));
    held.check_current().unwrap();
    std::fs::remove_file(r.join("not-a-directory")).unwrap();
    std::fs::create_dir(r.join("not-a-directory")).unwrap();
    assert!(held.check_current().is_err());
}

#[test]
fn cycles_special_inputs_writable_roots_and_syntax_bounds_refuse() {
    let (_tmp, r) = fixture();
    symlink("cycle", r.join("cycle")).unwrap();
    assert!(
        HeldCommandSearchInputs::capture(
            &r,
            &paths(&[r.join("cycle")]),
            OsStr::new("otool"),
            end()
        )
        .is_err()
    );
    std::fs::set_permissions(r.join("early"), std::fs::Permissions::from_mode(0o777)).unwrap();
    assert!(
        HeldCommandSearchInputs::capture(
            &r,
            &paths(&[r.join("early")]),
            OsStr::new("otool"),
            end()
        )
        .is_err()
    );
    for name in ["", ".", "..", "bin/otool"] {
        assert!(
            HeldCommandSearchInputs::capture(&r, OsStr::new(""), OsStr::new(name), end()).is_err()
        );
    }
    for raw in [
        vec![b'x'; 64 * 1024 + 1],
        b"bad\0path".to_vec(),
        vec![b':'; MAX_ROOTS],
    ] {
        assert!(
            HeldCommandSearchInputs::capture(
                &r,
                &OsString::from_vec(raw),
                OsStr::new("otool"),
                end()
            )
            .is_err()
        );
    }
    let fifo = r.join("late/fifo");
    let name = CString::new(fifo.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    assert!(
        HeldCommandSearchInputs::capture(&r, &paths(&[r.join("late")]), OsStr::new("fifo"), end())
            .is_err()
    );
}

#[test]
fn first_end_process_credentials_and_panicked_checks_cannot_be_replaced() {
    let (_tmp, r) = fixture();
    for fault in [0, 1, 2] {
        let held = capture(&r, &paths(&[r.join("late")]));
        {
            let mut original = held.inner.lock().unwrap();
            match fault {
                0 => original.end = Instant::now(),
                1 => original.creator = original.creator.wrapping_add(1),
                _ => original.credentials.groups.push(u32::MAX),
            }
        }
        assert!(held.check_current().is_err());
        assert!(held.check_current().is_err());
    }
    let held = capture(&r, &paths(&[r.join("late")]));
    let panic = std::panic::catch_unwind(|| {
        let _guard = held.inner.lock().unwrap();
        panic!("controlled owner-check panic");
    });
    assert!(panic.is_err());
    assert!(held.check_current().is_err());
}

#[test]
fn actual_fork_rejects_the_inherited_owner_before_a_held_mutex_or_allocator() {
    let (_tmp, r) = fixture();
    let held = capture(&r, &paths(&[r.join("late")]));
    let guard = held.inner.lock().unwrap();
    // The child exercises only the async-safe PID/inline-errno rejection. It
    // creates no production NoFork, Root, loan or native settlement authority.
    let child = unsafe { libc::fork() };
    assert!(child >= 0);
    if child == 0 {
        let refused = matches!(held.check_current(), Err(e) if e.raw_os_error()==Some(libc::EPERM))
            && matches!(held.retain_inputs(&[]), Err(e) if e.raw_os_error()==Some(libc::EPERM));
        unsafe { libc::_exit(if refused { 0 } else { 74 }) }
    }
    let until = end();
    let mut status = 0;
    loop {
        let result = unsafe { libc::waitpid(child, &mut status, libc::WNOHANG) };
        if result == child {
            break;
        }
        if result < 0 {
            assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::EINTR));
        }
        if Instant::now() >= until {
            // This still-unreaped direct child retains its exact PID; no
            // signalling occurs after a successful reap or outside this test.
            unsafe {
                libc::kill(child, libc::SIGKILL);
            }
            while unsafe { libc::waitpid(child, &mut status, 0) } < 0 {
                assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::EINTR));
            }
            panic!("foreign owner touched an inherited held mutex");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(libc::WIFEXITED(status));
    assert_eq!(libc::WEXITSTATUS(status), 0);
    drop(guard);
    held.check_current().unwrap();
}

#[test]
fn failed_real_descriptor_capture_releases_only_new_fds_and_keeps_the_original_caller() {
    const NAME: &str = "program::command_search::tests::failed_real_descriptor_capture_releases_only_new_fds_and_keeps_the_original_caller";
    const ACTOR: &str = "TRUST_COMMAND_SEARCH_EMFILE_ACTOR";
    if std::env::var_os(ACTOR).is_none() {
        let mut command = aterm_uds::spawnfd::command(std::env::current_exe().unwrap());
        command
            .args(["--exact", NAME, "--nocapture"])
            .env(ACTOR, "1");
        let output =
            aterm_uds::deadline::output_within(&mut command, Duration::from_secs(60)).unwrap();
        assert!(
            output.status.success(),
            "isolated descriptor actor failed: {output:?}"
        );
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("original-search-caller-retained")
        );
        return;
    }
    let (_tmp, r) = fixture();
    capture(&r, &paths(&[r.join("late")]));
    let caller = File::open(r.join("late/otool")).unwrap();
    let original = Stamp::metadata(&caller.metadata().unwrap());
    let mut limit = std::mem::MaybeUninit::uninit();
    assert_eq!(
        unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, limit.as_mut_ptr()) },
        0
    );
    let mut limit = unsafe { limit.assume_init() };
    limit.rlim_cur = limit.rlim_cur.min(128);
    assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &limit) }, 0);
    let mut own = Vec::new();
    loop {
        match File::open("/dev/null") {
            Ok(file) => own.push(file),
            Err(error) => {
                assert_eq!(error.raw_os_error(), Some(libc::EMFILE));
                break;
            }
        }
    }
    for _ in 0..4 {
        drop(own.pop().unwrap());
    }
    let count = || {
        (0..limit.rlim_cur as i32)
            .filter(|fd| unsafe { libc::fcntl(*fd, libc::F_GETFD) } >= 0)
            .count()
    };
    let before = count();
    let refused =
        HeldCommandSearchInputs::capture(&r, &paths(&[r.join("late")]), OsStr::new("otool"), end());
    assert_eq!(refused.err().unwrap().raw_os_error(), Some(libc::EMFILE));
    assert_eq!(
        count(),
        before,
        "failed transaction leaked a newly opened descriptor"
    );
    assert!(Stamp::metadata(&caller.metadata().unwrap()) == original);
    println!("original-search-caller-retained");
}
