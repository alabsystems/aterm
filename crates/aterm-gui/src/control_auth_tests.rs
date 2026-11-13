// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::native_document_host::HeldAdvisoryLock;

#[test]
fn client_named_paths_are_named_only_for_server_minted_ids() {
    let sock = Path::new("/c");
    assert_eq!(
        video_instance_root_for(sock, "p12-ab-3"),
        Some(sock.join(VIDEO_DIR).join("p12-ab-3"))
    );
    for bad in ["", ".", "..", "a/b", "a.b", "a\0b"] {
        assert_eq!(video_instance_root_for(sock, bad), None, "{bad:?}");
    }

    let dir = Path::new("/c/integration");
    let sid = aterm_session::SessionId::new("s-09035e075d6ee9388965".to_string());
    assert_eq!(
        sid_file_in(dir, &sid),
        Some(dir.join("s-09035e075d6ee9388965"))
    );
    for bad in ["../x", "s-", "s-zz", "s-12/../../etc", "abc"] {
        assert_eq!(
            sid_file_in(dir, &aterm_session::SessionId::new(bad.to_string())),
            None,
            "{bad}"
        );
    }
}

/// `socket_is_live` is true for a bound listener and false for an absent or stale socket,
/// so the bind gate removes stale files but never unlinks a live listener.
#[test]
fn socket_is_live_true_for_bound_false_for_stale() {
    if !crate::test_alone::alone() {
        return;
    }
    let dir = crate::test_scratch::short_dir("the probed socket");
    let path = dir.path().join("s.sock");
    let ps = path.to_string_lossy().into_owned();
    assert!(!socket_is_live(&ps), "absent socket is not live");
    let listener = aterm_uds::CtlListener::bind(&path).expect("bind");
    assert!(socket_is_live(&ps), "bound socket is live");
    drop(listener);
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "stale socket file (no listener) becomes not-live",
        || (!socket_is_live(&ps)).then_some(()),
    );
}

/// A liveness probe must remain bounded even when nobody drains accept().
#[cfg(unix)]
#[test]
fn socket_teardown_wait_does_not_block_on_a_full_accept_backlog() {
    use std::os::fd::AsRawFd;
    let dir = crate::test_scratch::short_dir("the control socket");
    let path = dir.path().join("backlog.sock");
    let listener = aterm_uds::CtlListener::bind(&path).unwrap();
    // SAFETY: lower the backlog of this test's own live listener.
    assert_eq!(unsafe { libc::listen(listener.as_raw_fd(), 1) }, 0);
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    let path = path.to_str().unwrap().to_string();
    let probe = std::thread::spawn(move || {
        let initially_live = connect_socket_nonblocking(&path).is_ok();
        for _ in 0..128 {
            let _ = connect_socket_nonblocking(&path);
        }
        let _ = tx.send(initially_live);
    });
    let result = rx.recv_timeout(aterm_spec::wait::MUST_HAPPEN);
    drop(listener);
    probe.join().unwrap();
    assert_eq!(
        result,
        Ok(true),
        "teardown hint blocked or missed the initial live listener"
    );
}

/// The teardown dial is close-on-exec and non-blocking.
#[cfg(unix)]
#[test]
fn the_teardown_dial_is_close_on_exec_and_non_blocking() {
    use std::os::fd::AsRawFd;
    let dir = crate::test_scratch::short_dir("the control socket");
    let path = dir.path().join("cloexec.sock");
    let listener = aterm_uds::CtlListener::bind(&path).unwrap();
    let stream = connect_socket_nonblocking(path.to_str().unwrap()).unwrap();

    // SAFETY: both reads target this test's own live descriptor.
    let fd_flags = unsafe { libc::fcntl(stream.as_raw_fd(), libc::F_GETFD) };
    let status_flags = unsafe { libc::fcntl(stream.as_raw_fd(), libc::F_GETFL) };
    assert!(
        fd_flags >= 0 && fd_flags & libc::FD_CLOEXEC != 0,
        "the dial is inherited across exec: F_GETFD = {fd_flags:#x}"
    );
    assert!(
        status_flags >= 0 && status_flags & libc::O_NONBLOCK != 0,
        "the dial blocks: F_GETFL = {status_flags:#x}"
    );
    drop(stream);

    for (bad, kind) in [
        (
            format!("{}\0junk", path.to_str().unwrap()),
            std::io::ErrorKind::InvalidInput,
        ),
        (
            "a".repeat(MAX_SUN_PATH + 1),
            std::io::ErrorKind::InvalidInput,
        ),
        ("a".repeat(MAX_SUN_PATH), std::io::ErrorKind::NotFound),
    ] {
        let refused = connect_socket_nonblocking(&bad).expect_err("no listener");
        assert_eq!(refused.kind(), kind, "{bad:?}");
    }

    let live = path.to_str().unwrap();
    let absent = dir.path().join("absent.sock");
    let absent = absent.to_str().unwrap();
    for (is_live, want) in [
        (socket_is_live(live), BindAction::RefuseLiveSocket),
        (
            socket_is_live_per_instance(live),
            BindAction::RefuseLiveSocket,
        ),
        (socket_is_live(absent), BindAction::RemoveAndBind),
        (
            socket_is_live_per_instance(absent),
            BindAction::RemoveAndBind,
        ),
    ] {
        assert_eq!(decide_bind(is_live), want);
    }
    drop(listener);
}

/// The `sun_path` limit is counted in bytes; an explicit path over it is refused by name
/// ([`SocketResolution::PathTooLong`]) before any bind or probe.
#[test]
fn sun_path_limit_is_enforced_byte_wise() {
    assert!(sun_path_ok(&"a".repeat(MAX_SUN_PATH)), "at the limit binds");
    assert!(
        !sun_path_ok(&"a".repeat(MAX_SUN_PATH + 1)),
        "one over is refused"
    );
    let mb = format!("{}é", "a".repeat(MAX_SUN_PATH - 1));
    assert!(!sun_path_ok(&mb), "multibyte overflow is caught byte-wise");
    #[cfg(target_os = "macos")]
    assert_eq!(MAX_SUN_PATH, 103);
    #[cfg(target_os = "linux")]
    assert_eq!(MAX_SUN_PATH, 107);
}

/// Each way the launch can end up socketless is its own named outcome,
/// and each bindable plan pairs the socket with its own token.
#[test]
fn the_socket_plan_names_every_refusal() {
    use SocketDirective::{Disabled, Explicit, PerInstance};
    let temp = crate::test_scratch::short_dir("the explicit socket");
    let short = temp.path().join("a.sock").to_string_lossy().into_owned();
    let SocketResolution::Enabled(plan) = resolve_socket_plan_for(Explicit(short.clone()), || None)
    else {
        panic!("a short explicit path binds");
    };
    assert_eq!(
        (plan.sock_path.as_str(), plan.latest_link),
        (short.as_str(), None)
    );
    assert_eq!(plan.token_path, token_path_for_socket(&short));
    let long = format!("/{}", "a".repeat(MAX_SUN_PATH));
    assert!(matches!(
        resolve_socket_plan_for(Explicit(long.clone()), || None),
        SocketResolution::PathTooLong { path, limit } if path == long && limit == MAX_SUN_PATH
    ));
    assert!(matches!(
        resolve_socket_plan_for(Disabled, || None),
        SocketResolution::Disabled
    ));
    assert!(matches!(
        resolve_socket_plan_for(PerInstance, || None),
        SocketResolution::NoDir
    ));
    let not_a_dir = temp.path().join("file");
    std::fs::write(&not_a_dir, b"x").unwrap();
    assert!(matches!(
        resolve_socket_plan_for(PerInstance, || Some(not_a_dir.clone())),
        SocketResolution::DirUnusable { dir, .. } if dir == not_a_dir
    ));
    let dir = temp.path().join("control");
    let SocketResolution::Enabled(plan) =
        resolve_socket_plan_for(PerInstance, || Some(dir.clone()))
    else {
        panic!("a private per-user directory binds");
    };
    let pid = std::process::id();
    assert_eq!(
        plan.sock_path,
        dir.join(control_socket::instance_sock_name(pid))
            .to_string_lossy()
    );
    assert_eq!(
        plan.token_path,
        dir.join(control_socket::instance_token_name(pid))
    );
    assert_eq!(plan.latest_link, Some(dir.join(SOCK_FILE)));
}

/// The best-effort cleanup queue admits exactly its limit
/// ([`crate::control::ARTIFACT_CLEANUP_QUEUE_LIMIT`]), a permit's clones (a
/// panic's recovery copy) hold one place between them, and the place is
/// admitted again once the last of them drops.
#[test]
fn the_cleanup_queue_admits_exactly_its_limit_and_a_freed_place_again() {
    let budgets = crate::control::ArtifactBudgets::shipping();
    let reserve = || ArtifactCleanupQueuePermit::try_reserve(&budgets);
    let mut permits: Vec<_> = (0..crate::control::ARTIFACT_CLEANUP_QUEUE_LIMIT)
        .map(|_| reserve().expect("within the limit"))
        .collect();
    assert!(reserve().is_none(), "one past the limit is refused");
    let held = permits.pop().expect("an admitted place");
    let recovery = held.clone();
    drop(held);
    assert!(
        reserve().is_none(),
        "the recovery copy still holds the place"
    );
    drop(recovery);
    permits.push(reserve().expect("a freed place"));
    assert!(reserve().is_none());
}

#[test]
fn random_token_is_64_hex_chars() {
    let t = random_token_hex().expect("entropy available");
    assert_eq!(t.len(), 64);
    assert!(
        t.chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
    );
    assert_eq!(
        check_auth_line(&format!("AUTH {t}"), &t),
        AuthOutcome::Ok(None)
    );
}

/// `AUTH <token>` and `TOKEN <token> [verb]` admit the exact token only
/// (a trailing CR tolerated); anything else is denied.
#[test]
fn auth_line_admits_only_the_exact_token() {
    let tok = "a".repeat(64);
    let bad = "b".repeat(64);
    for (line, want) in [
        (format!("AUTH {tok}"), AuthOutcome::Ok(None)),
        (format!("AUTH {tok}\r"), AuthOutcome::Ok(None)),
        (format!("AUTH {tok} "), AuthOutcome::Ok(None)),
        (format!("AUTH {bad}"), AuthOutcome::Denied),
        ("AUTH".to_string(), AuthOutcome::Denied),
        ("text".to_string(), AuthOutcome::Denied),
        (String::new(), AuthOutcome::Denied),
        (
            format!("TOKEN {tok} text"),
            AuthOutcome::Ok(Some("text".to_string())),
        ),
        (
            format!("TOKEN {tok} send echo hi"),
            AuthOutcome::Ok(Some("send echo hi".to_string())),
        ),
        (format!("TOKEN {tok}"), AuthOutcome::Ok(Some(String::new()))),
        (format!("TOKEN {bad} text"), AuthOutcome::Denied),
        (
            format!("TOKEN {tok} text\r"),
            AuthOutcome::Ok(Some("text".to_string())),
        ),
        (format!("AUTH  {tok}"), AuthOutcome::Denied),
        ("TOKEN".to_string(), AuthOutcome::Denied),
        ("TOKEN  text".to_string(), AuthOutcome::Denied),
        (format!("TOKEN {} text", &tok[..63]), AuthOutcome::Denied),
        (
            format!("AUTH {tok}{}", "\0".repeat(65_536)),
            AuthOutcome::Denied,
        ),
        (
            format!("TOKEN {tok}{} text", "\0".repeat(65_536)),
            AuthOutcome::Denied,
        ),
    ] {
        assert_eq!(check_auth_line(&line, &tok), want, "{line:?}");
    }
    let line = format!("AUTH {tok}");
    assert_eq!(check_line_against(&line, Some(&tok)), AuthOutcome::Ok(None));
    assert_eq!(check_line_against(&line, Some(&bad)), AuthOutcome::Denied);
    assert_eq!(check_line_against(&line, None), AuthOutcome::Denied);
    assert_eq!(check_line_against("AUTH ", None), AuthOutcome::Denied);
}

/// Our own loosened directory is forced back to exactly `0700`
/// (special bits too), and a directory that is not ours is REFUSED, even
/// one already `0700`: a chmod cannot change its owner.
#[test]
#[cfg(unix)]
fn ensure_private_dir_hardens_ours_and_refuses_a_strangers() {
    use std::os::unix::fs::PermissionsExt;
    let temp = aterm_tempfile::tempdir().unwrap();
    let dir = temp.path().join("control");
    let mode = || std::fs::metadata(&dir).unwrap().permissions().mode() & 0o7777;
    ensure_private_dir(&dir).unwrap();
    for loose in [0o770, 0o1700] {
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(loose)).unwrap();
        ensure_private_dir(&dir).unwrap();
        assert_eq!(mode(), 0o700, "{loose:o} is forced to 0700");
    }
    let stranger = our_uid().wrapping_add(1);
    for planted in [0o700, 0o755] {
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(planted)).unwrap();
        let refused = imp::ensure_private_dir_as(&dir, stranger)
            .expect_err("a directory owned by another uid is never private");
        assert_eq!(
            refused.kind(),
            std::io::ErrorKind::PermissionDenied,
            "{planted:o}"
        );
    }
}

#[test]
#[cfg(unix)]
fn provision_token_refuses_symlinked_path() {
    use std::os::unix::fs::symlink;
    let dir = std::env::temp_dir().join(format!("aterm-tok-sym-{}", std::process::id()));
    ensure_private_dir(&dir).unwrap();
    let victim = dir.join("victim.txt");
    std::fs::write(&victim, b"original").unwrap();
    let tokpath = dir.join("aterm.token");
    symlink(&victim, &tokpath).unwrap();
    let _ = provision_token(&tokpath);
    assert_eq!(
        std::fs::read(&victim).unwrap(),
        b"original",
        "the symlink target must not be written through",
    );
    let md = std::fs::symlink_metadata(&tokpath).unwrap();
    assert!(
        md.file_type().is_file(),
        "token path must be a regular file"
    );
    std::fs::remove_file(&tokpath).unwrap();
    std::fs::hard_link(&victim, &tokpath).unwrap();
    assert_eq!(imp::write_fresh_token(&tokpath, "token"), None);
    assert_eq!(std::fs::read(&victim).unwrap(), b"original");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
#[cfg(unix)]
fn provision_and_read_token_roundtrip() {
    use std::os::unix::fs::PermissionsExt;
    let dir = std::env::temp_dir().join(format!("aterm-auth-test-{}", std::process::id()));
    ensure_private_dir(&dir).unwrap();
    let sock = dir.join("a.sock").to_string_lossy().into_owned();
    let token_path = token_path_for_socket(&sock);
    assert_eq!(token_path, dir.join("a.sock.token"));
    let written = provision_token(&token_path).expect("token written");
    let read = read_token(&sock).expect("token readable");
    assert_eq!(written, read);
    let other = dir.join("b.sock").to_string_lossy().into_owned();
    let other_written = provision_token(&token_path_for_socket(&other)).expect("token written");
    assert_ne!(other_written, written);
    assert_eq!(read_token(&sock).as_deref(), Some(written.as_str()));
    assert_eq!(read_token(&other).as_deref(), Some(other_written.as_str()));
    let mode = std::fs::metadata(&token_path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    let dmode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
    assert_eq!(dmode, 0o700);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Windows twin of the Unix roundtrip and symlink tests: `create_new` refuses any
/// pre-existing object at the path, and `provision_token`'s unlink-first still lets a
/// rotation replace our own stale token.
#[test]
#[cfg(windows)]
fn provision_token_roundtrips_and_create_new_refuses_preexisting() {
    let dir = std::env::temp_dir().join(format!("aterm-auth-test-{}", std::process::id()));
    ensure_private_dir(&dir).unwrap();
    let sock = dir.join("a.sock").to_string_lossy().into_owned();
    let tokpath = token_path_for_socket(&sock);
    std::fs::write(&tokpath, b"planted").unwrap();
    let err = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tokpath)
        .expect_err("CREATE_NEW must refuse a pre-existing object");
    assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
    let written = provision_token(&tokpath).expect("token written");
    let read = read_token(&sock).expect("token readable");
    assert_eq!(written, read);
    assert_eq!(written.len(), 64);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn confine_image_path_allows_inside_and_rejects_escape() {
    let dir = std::env::temp_dir().join(format!("aterm-img-test-{}", std::process::id()));
    ensure_private_dir(&dir).unwrap();

    let ok = confine_image_path(&dir, "shot.png").expect("bare name allowed");
    assert_eq!(ok.file_name.to_str(), Some("shot.png"));
    assert!(
        ok.display_path().ends_with("images/shot.png"),
        "got {:?}",
        ok.display_path()
    );

    assert!(confine_image_path(&dir, "../escape.png").is_none());
    assert!(confine_image_path(&dir, "../../etc/passwd").is_none());

    assert!(confine_image_path(&dir, "/tmp/evil.png").is_none());

    let inside = dir.join(IMAGES_DIR).join("ok.png");
    let allowed =
        confine_image_path(&dir, inside.to_str().unwrap()).expect("absolute-inside allowed");
    assert!(allowed.display_path().ends_with("images/ok.png"));

    let above_root = format!(
        "/..{}",
        std::fs::canonicalize(dir.join(IMAGES_DIR))
            .unwrap()
            .join("ok.png")
            .display()
    );
    for refused in [
        AUTO_IMAGES_DIR,
        CAPTURE_LOCK_DIR,
        "a\0b.png",
        above_root.as_str(),
    ] {
        assert!(confine_image_path(&dir, refused).is_none(), "{refused:?}");
    }
    for (path, want) in [
        ("/a/./b/../c", Some("/a/c")),
        ("/..", None),
        ("/a/../../b", None),
    ] {
        assert_eq!(
            lexically_normalize(Path::new(path)),
            want.map(PathBuf::from),
            "{path}"
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn image_confinement_decision_is_distinct_from_capacity_admission() {
    let dir = std::env::temp_dir().join(format!(
        "aterm-img-confine-admission-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    ensure_private_dir(&dir).unwrap();

    let (images, name) = resolve_confined_image_target(&dir, "shot.png")
        .expect("the pure confinement decision accepts a direct child");
    assert_eq!(images, dir.join(IMAGES_DIR).canonicalize().unwrap());
    assert_eq!(name, std::ffi::OsStr::new("shot.png"));

    let mut admission_checked = false;
    let rejected = confine_image_path_with_admission(&dir, "shot.png", |_| {
        admission_checked = true;
        false
    });
    assert!(admission_checked, "capacity is consulted after confinement");
    assert!(
        matches!(rejected, Err(ArtifactConfinementError::AdmissionRefused)),
        "a confined path reports resource refusal distinctly"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn confine_image_path_rejects_nested_target_dir() {
    let dir = std::env::temp_dir().join(format!("aterm-img-nested-{}", std::process::id()));
    ensure_private_dir(&dir).unwrap();
    let images = dir.join(IMAGES_DIR);
    ensure_private_dir(&images).unwrap();
    ensure_private_dir(&images.join("sub")).unwrap();
    assert!(
        confine_image_path(&dir, "sub/shot.png").is_none(),
        "a nested target dir must be rejected (intermediate-dir TOCTOU)"
    );
    let nested_abs = images.join("sub").join("shot.png");
    assert!(confine_image_path(&dir, nested_abs.to_str().unwrap()).is_none());
    assert!(confine_image_path(&dir, "shot.png").is_some());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
#[cfg(unix)]
fn confine_image_path_rejects_symlinked_final_component() {
    use std::os::unix::fs::symlink;
    let dir = std::env::temp_dir().join(format!("aterm-img-symlink-{}", std::process::id()));
    ensure_private_dir(&dir).unwrap();
    let images = dir.join(IMAGES_DIR);
    ensure_private_dir(&images).unwrap();
    let victim = dir.join("victim.txt");
    std::fs::write(&victim, b"original").unwrap();
    symlink(&victim, images.join("evil.png")).unwrap();

    assert!(
        confine_image_path(&dir, "evil.png").is_none(),
        "a symlinked final component must be rejected (arbitrary-write escape)"
    );
    assert!(confine_image_path(&dir, "fresh.png").is_some());
    std::fs::write(images.join("real.png"), b"x").unwrap();
    assert!(confine_image_path(&dir, "real.png").is_some());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn automatic_images_are_unique_bounded_and_never_prune_explicit_targets() {
    let dir = std::env::temp_dir().join(format!("aterm-img-auto-retention-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    ensure_private_dir(&dir).unwrap();
    let explicit = confine_image_path(&dir, "operator-kept.png").unwrap();
    std::fs::write(explicit.display_path(), b"explicit").unwrap();
    for stem in ["", "a/b", "a.b"] {
        assert!(
            confine_automatic_image_path(&dir, stem).is_none(),
            "{stem:?}"
        );
    }

    let mut names = std::collections::HashSet::new();
    let mut auto_dir = None;
    for _ in 0..(AUTO_IMAGE_KEEP + 7) {
        let target = confine_automatic_image_path(&dir, "image").unwrap();
        assert!(
            names.insert(target.file_name.clone()),
            "every omitted-name request gets a fresh server name"
        );
        std::fs::write(target.display_path(), b"png").unwrap();
        prune_automatic_image_dir(&target);
        auto_dir = Some(target.dir);
    }
    let auto_dir = auto_dir.unwrap();
    let captures = || {
        std::fs::read_dir(&auto_dir)
            .unwrap()
            .flatten()
            .filter_map(|entry| {
                Some((
                    automatic_capture_sequence(&entry.file_name())?,
                    entry.path(),
                ))
            })
            .collect::<std::collections::BTreeMap<_, _>>()
    };
    assert_eq!(
        captures().len(),
        AUTO_IMAGE_KEEP,
        "successful auto captures converge to the retention cap"
    );
    #[cfg(target_os = "macos")]
    {
        let held = captures().into_values().collect::<Vec<_>>();
        let _undeletable = Immutable::set(&held[0]);
        let target = confine_automatic_image_path(&dir, "image").unwrap();
        std::fs::write(target.display_path(), b"png").unwrap();
        prune_automatic_image_dir(&target);
        let left = captures().into_values().collect::<Vec<_>>();
        assert_eq!(left.len(), AUTO_IMAGE_KEEP);
        assert!(left.contains(&held[0]) && !left.contains(&held[1]));
    }
    assert_eq!(
        std::fs::read(explicit.display_path()).unwrap(),
        b"explicit",
        "automatic retention never reaches caller-explicit files"
    );

    let partial = confine_automatic_image_path(&dir, "window").unwrap();
    std::fs::write(partial.display_path(), b"partial").unwrap();
    cleanup_failed_automatic_image(&partial);
    assert!(
        !partial.display_path().exists(),
        "failed automatic writes remove their partial target"
    );
    cleanup_failed_automatic_image(&explicit);
    assert_eq!(
        std::fs::read(explicit.display_path()).unwrap(),
        b"explicit",
        "failure cleanup is structurally unable to unlink explicit targets"
    );
    let auto_sock = dir.join(AUTO_IMAGES_DIR);
    ensure_private_dir(&auto_sock).unwrap();
    let lookalike = confine_image_path(
        &auto_sock,
        &automatic_capture_name_for("image", process_instance_id(), 1),
    )
    .unwrap();
    std::fs::write(lookalike.display_path(), b"explicit").unwrap();
    cleanup_failed_automatic_image(&lookalike);
    assert!(lookalike.display_path().is_file());
    drop(explicit.write_private(b"again").unwrap());
    assert_eq!(std::fs::read(explicit.display_path()).unwrap(), b"again");
    let minted = confine_automatic_image_path(&dir, "image").unwrap();
    std::fs::write(minted.display_path(), b"planted").unwrap();
    let refused = minted.write_private(b"png").unwrap_err();
    assert_eq!(refused.kind(), std::io::ErrorKind::AlreadyExists);
    assert_eq!(std::fs::read(minted.display_path()).unwrap(), b"planted");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn capture_namespace_default_waits_for_reply_release() {
    let dir = std::env::temp_dir().join(format!("aterm-img-lease-wake-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    ensure_private_dir(&dir).unwrap();

    let first = ConfinedImage::for_test(&dir, "first.png");
    let first_lease = acquire_capture_name_lease(&first, || false)
        .expect("first lease acquisition")
        .expect("first explicit lease");
    let expired = std::time::Instant::now() - std::time::Duration::from_secs(61);
    assert_eq!(
        acquire_capture_name_lease_started(&first, || false, expired)
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::WouldBlock
    );
    assert!(
        acquire_capture_name_lease_started(&first, || true, expired)
            .unwrap()
            .is_none()
    );
    let second = ConfinedImage::for_test(&dir, "second.png");
    let observed_wait = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let worker_observed_wait = std::sync::Arc::clone(&observed_wait);
    let waiter = std::thread::spawn(move || {
        acquire_capture_name_lease_started(
            &second,
            || {
                worker_observed_wait.store(true, std::sync::atomic::Ordering::Release);
                false
            },
            std::time::Instant::now() - std::time::Duration::from_secs(3),
        )
    });
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the successor observes the retained reply lease",
        || {
            observed_wait
                .load(std::sync::atomic::Ordering::Acquire)
                .then_some(())
        },
    );

    drop(first_lease);
    let second_lease = waiter
        .join()
        .expect("lease waiter remains live")
        .expect("released predecessor wakes its waiter")
        .expect("waiter acquires the explicit namespace");
    drop(second_lease);
    drop(
        acquire_capture_name_lease(&first, || false)
            .unwrap()
            .expect("a fresh owner"),
    );
    drop(first);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn explicit_capture_wait_cancellation_wins() {
    let dir = std::env::temp_dir().join(format!("aterm-img-lease-cancel-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    ensure_private_dir(&dir).unwrap();

    let first = ConfinedImage::for_test(&dir, "first.png");
    let first_lease = acquire_capture_name_lease(&first, || false)
        .expect("first lease acquisition")
        .expect("first explicit lease");
    let second = ConfinedImage::for_test(&dir, "second.png");
    let cancelled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let observed_wait = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let worker_cancelled = std::sync::Arc::clone(&cancelled);
    let worker_observed_wait = std::sync::Arc::clone(&observed_wait);
    let waiter = std::thread::spawn(move || {
        acquire_capture_name_lease_with_wait(
            &second,
            || {
                worker_observed_wait.store(true, std::sync::atomic::Ordering::Release);
                worker_cancelled.load(std::sync::atomic::Ordering::Acquire)
            },
            aterm_spec::wait::MUST_HAPPEN,
        )
    });
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the cancellation probe reaches the wait",
        || {
            observed_wait
                .load(std::sync::atomic::Ordering::Acquire)
                .then_some(())
        },
    );
    cancelled.store(true, std::sync::atomic::Ordering::Release);

    assert!(
        waiter
            .join()
            .expect("lease waiter remains live")
            .expect("cancellation is not an I/O failure")
            .is_none(),
        "cancellation wins without acquiring the retained namespace"
    );
    drop(first_lease);

    let probes = std::cell::Cell::new(0);
    let late = acquire_capture_name_lease(&first, || {
        probes.set(probes.get() + 1);
        probes.get() > 1
    });
    assert!(late.expect("cancellation is not an I/O failure").is_none());
    let image_lease =
        acquire_capture_name_lease_with_wait(&first, || false, std::time::Duration::ZERO)
            .expect("the cancelled reservation let its namespace go")
            .expect("explicit lease");
    let unrelated = crate::pinned_dir::PinnedDir::open_resolved(&dir).unwrap();
    assert_eq!(
        image_lease
            .arm_video_retention_sweep(&unrelated)
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::InvalidInput
    );
    drop(image_lease);
    drop(first);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn explicit_capture_stuck_holder_returns_busy_after_bound() {
    let dir = std::env::temp_dir().join(format!("aterm-img-lease-bounded-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    ensure_private_dir(&dir).unwrap();

    let first = ConfinedImage::for_test(&dir, "first.png");
    let first_lease = acquire_capture_name_lease(&first, || false)
        .expect("first lease acquisition")
        .expect("first explicit lease");
    let second = ConfinedImage::for_test(&dir, "second.png");
    let error = acquire_capture_name_lease_with_wait(
        &second,
        || false,
        std::time::Duration::from_millis(40),
    )
    .expect_err("a stuck predecessor must not park the encode worker");
    assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock);

    drop(first_lease);
    drop(first);
    drop(second);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Another process's hold on the shared image namespace (its own open file description
/// of the directory) gets the same bounded wait as this process's own, and past it the
/// refusal says why.
#[test]
#[cfg(unix)]
fn capture_namespace_default_waits_for_native_sibling() {
    let dir = std::env::temp_dir().join(format!("aterm-img-sibling-lock-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    ensure_private_dir(&dir).unwrap();
    let image = ConfinedImage::for_test(&dir, "sibling.png");
    let sibling = HeldAdvisoryLock::try_lock(
        image
            .pinned()
            .unwrap()
            .open_directory_lock()
            .expect("the sibling's own description of the directory"),
    )
    .expect("the sibling holds the namespace");
    let expired = std::time::Instant::now() - std::time::Duration::from_secs(61);
    assert_eq!(
        acquire_capture_name_lease_started(&image, || false, expired)
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::WouldBlock
    );
    assert!(
        acquire_capture_name_lease_started(&image, || true, expired)
            .unwrap()
            .is_none()
    );
    let error = acquire_capture_name_lease_with_wait(
        &image,
        || false,
        std::time::Duration::from_millis(60),
    )
    .expect_err("a sibling that never lets go is reported, not waited on forever");
    assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock);
    assert!(
        error.to_string().contains("not acknowledged"),
        "the refusal names the cause: {error}"
    );
    let asks = std::sync::atomic::AtomicUsize::new(0);
    let lease = acquire_capture_name_lease_started(
        &image,
        || {
            if asks.fetch_add(1, std::sync::atomic::Ordering::Relaxed) == 1 {
                sibling.file().unlock().expect("the sibling lets go");
            }
            false
        },
        std::time::Instant::now() - std::time::Duration::from_secs(3),
    )
    .expect("the queued capture proceeds once the sibling lets go")
    .expect("not cancelled");
    assert!(
        asks.load(std::sync::atomic::Ordering::Relaxed) >= 2,
        "the capture queued behind the sibling before it proceeded"
    );
    drop(lease);
    drop(sibling);
    drop(
        acquire_capture_name_lease(&image, || false)
            .unwrap()
            .expect("a fresh native owner"),
    );
    drop(image);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
#[cfg(unix)]
fn explicit_capture_release_unlocks_an_inherited_descriptor() {
    let dir = std::env::temp_dir().join(format!("aterm-img-inherited-lock-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    ensure_private_dir(&dir).unwrap();

    let first = ConfinedImage::for_test(&dir, "first.png");
    let first_lease = acquire_capture_name_lease(&first, || false)
        .expect("first lease acquisition")
        .expect("first explicit lease");
    let inherited = first_lease
        .os_lock
        .as_ref()
        .expect("explicit Unix lease owns an OS lock")
        .file()
        .try_clone()
        .expect("duplicate the descriptor as fork would");
    drop(first_lease);

    let second = ConfinedImage::for_test(&dir, "second.png");
    let second_lease = acquire_capture_name_lease(&second, || false)
        .expect("release must not leave a lock in an inherited descriptor")
        .expect("second explicit lease");
    drop(second_lease);
    drop(inherited);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn automatic_image_retention_preserves_every_queued_wire_reply() {
    let dir = std::env::temp_dir().join(format!(
        "aterm-img-auto-queued-retention-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    ensure_private_dir(&dir).unwrap();

    let queued = confine_automatic_image_path(&dir, "image").unwrap();
    let queued_path = queued.display_path();
    let queued_lease = acquire_capture_name_lease(&queued, || false)
        .expect("automatic lease acquisition")
        .expect("lease queued capture");
    drop(queued.write_private(b"queued").unwrap());
    assert_eq!(mutate_unleased_artifact(&queued_path, || ()), None);

    for _ in 0..(AUTO_IMAGE_KEEP + 3) {
        let later = confine_automatic_image_path(&dir, "image").unwrap();
        std::fs::write(later.display_path(), b"later").unwrap();
    }
    let fresh = confine_automatic_image_path(&dir, "image").unwrap();
    std::fs::write(fresh.display_path(), b"fresh").unwrap();
    prune_automatic_image_dir(&fresh);

    assert_eq!(
        std::fs::read(&queued_path).unwrap(),
        b"queued",
        "a later reply's retention sweep cannot delete an older queued reply"
    );
    let completed = std::fs::read_dir(&fresh.dir)
        .unwrap()
        .flatten()
        .filter(|entry| automatic_capture_sequence(&entry.file_name()).is_some())
        .count();
    assert_eq!(
        completed, AUTO_IMAGE_KEEP,
        "leased replies consume retention slots without exceeding the cap"
    );

    drop(queued_lease);
    let next = confine_automatic_image_path(&dir, "image").unwrap();
    std::fs::write(next.display_path(), b"next").unwrap();
    prune_automatic_image_dir(&next);
    assert!(
        !queued_path.exists(),
        "after the wire lease drops, the oldest artifact becomes eligible"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn automatic_image_prune_progresses_past_one_scan_window() {
    let dir = std::env::temp_dir().join(format!(
        "aterm-img-auto-large-retention-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    ensure_private_dir(&dir).unwrap();
    let target = confine_automatic_image_path(&dir, "image").unwrap();
    std::fs::write(target.display_path(), b"fresh").unwrap();
    for sequence in 0..(AUTO_IMAGE_SCAN_BUDGET + 97) {
        let name = automatic_capture_name_for("image", process_instance_id(), sequence as u64);
        std::fs::write(target.dir.join(name), b"png").unwrap();
    }

    for _ in 0..8 {
        prune_automatic_image_dir(&target);
    }
    let remaining = std::fs::read_dir(&target.dir)
        .unwrap()
        .flatten()
        .filter(|entry| automatic_capture_sequence(&entry.file_name()).is_some())
        .count();
    assert_eq!(
        remaining, AUTO_IMAGE_KEEP,
        "a directory larger than one scan window converges under repeated bounded sweeps"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
#[cfg(unix)]
fn media_roots_refuse_planted_symlinks() {
    use std::os::unix::fs::symlink;

    let outside =
        std::env::temp_dir().join(format!("aterm-media-roots-outside-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&outside);
    ensure_private_dir(&outside).unwrap();

    let image_sock =
        std::env::temp_dir().join(format!("aterm-media-roots-image-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&image_sock);
    ensure_private_dir(&image_sock).unwrap();
    symlink(&outside, image_sock.join(IMAGES_DIR)).unwrap();
    assert!(
        confine_image_path(&image_sock, "shot.png").is_none(),
        "the explicit image root cannot redirect"
    );

    let video_sock =
        std::env::temp_dir().join(format!("aterm-media-roots-video-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&video_sock);
    ensure_private_dir(&video_sock).unwrap();
    symlink(&outside, video_sock.join(VIDEO_DIR)).unwrap();
    assert!(
        confine_video_dir(&video_sock).is_none(),
        "the video root cannot redirect"
    );

    let auto_sock =
        std::env::temp_dir().join(format!("aterm-media-roots-auto-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&auto_sock);
    ensure_private_dir(&auto_sock).unwrap();
    let images = ensure_canonical_direct_child(&auto_sock, IMAGES_DIR).unwrap();
    symlink(&outside, images.join(AUTO_IMAGES_DIR)).unwrap();
    assert!(
        confine_automatic_image_path(&auto_sock, "image").is_none(),
        "the automatic image root cannot redirect"
    );

    let lease_sock =
        std::env::temp_dir().join(format!("aterm-media-roots-lease-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&lease_sock);
    ensure_private_dir(&lease_sock).unwrap();
    let video = ensure_canonical_direct_child(&lease_sock, VIDEO_DIR).unwrap();
    let images = ensure_canonical_direct_child(&lease_sock, IMAGES_DIR).unwrap();
    let auto = ensure_canonical_direct_child(&images, AUTO_IMAGES_DIR).unwrap();
    for namespace in [video.join("p1-planted"), auto.join(process_instance_id())] {
        std::fs::create_dir(&namespace).unwrap();
        symlink(outside.join("lease"), namespace.join(INSTANCE_LEASE_FILE)).unwrap();
    }
    assert!(confine_video_dir_for_instance(&lease_sock, "p1-planted").is_none());
    assert!(confine_automatic_image_path(&lease_sock, "image").is_none());
    assert!(!outside.join("lease").exists());

    let _ = std::fs::remove_dir_all(lease_sock);
    let _ = std::fs::remove_dir_all(image_sock);
    let _ = std::fs::remove_dir_all(video_sock);
    let _ = std::fs::remove_dir_all(auto_sock);
    let _ = std::fs::remove_dir_all(outside);
}

#[test]
#[cfg(unix)]
fn confine_proxy_sock_allows_in_dir_and_rejects_redirects() {
    use std::os::unix::fs::symlink;
    let dir = std::env::temp_dir().join(format!("aterm-proxysock-{}", std::process::id()));
    ensure_private_dir(&dir).unwrap();
    let canon = std::fs::canonicalize(&dir).unwrap();

    let legit = dir.join("aterm-42.sock");
    let got = confine_proxy_sock(&dir, &legit.to_string_lossy()).expect("in-dir socket allowed");
    assert_eq!(got, canon.join("aterm-42.sock").to_string_lossy());

    assert!(confine_proxy_sock(&dir, "/tmp/evil-attacker.sock").is_none());
    let escape = dir.join("../evil.sock");
    assert!(confine_proxy_sock(&dir, &escape.to_string_lossy()).is_none());
    let cwd = std::env::current_dir().unwrap();
    let sub = std::fs::read_dir(&cwd)
        .unwrap()
        .flatten()
        .find(|entry| {
            entry.file_type().is_ok_and(|kind| kind.is_dir())
                && entry.file_name().to_str().is_some()
        })
        .expect("the test's cwd has a subdirectory")
        .file_name();
    let relative = Path::new(&sub).join("aterm-42.sock");
    let in_cwd = cwd.join(&sub);
    assert!(confine_proxy_sock(&in_cwd, &cwd.join(&relative).to_string_lossy()).is_some());
    assert!(
        confine_proxy_sock(&in_cwd, relative.to_str().unwrap()).is_none(),
        "relative rejected"
    );
    let nested = dir.join("sub/aterm-42.sock");
    assert!(confine_proxy_sock(&dir, &nested.to_string_lossy()).is_none());
    let nul = format!("{}/aterm\0.sock", dir.display());
    assert!(confine_proxy_sock(&dir, &nul).is_none());

    symlink("/tmp/evil-attacker.sock", dir.join("aterm-redir.sock")).unwrap();
    assert!(
        confine_proxy_sock(&dir, &dir.join("aterm-redir.sock").to_string_lossy()).is_none(),
        "a symlinked socket name must be rejected (token-capture escape)",
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
#[cfg(unix)]
fn peer_uid_of_socketpair_is_our_uid() {
    let (a, _b) = CtlStream::pair().expect("socketpair");
    assert_eq!(peer_uid(&a), Some(our_uid()));
    assert_eq!(peer_check(&a), Ok(()));
    assert_eq!(imp::peer_verdict(Some(501), 501), Ok(()));
    for stranger in [Some(502), Some(0), None] {
        assert!(imp::peer_verdict(stranger, 501).is_err(), "{stranger:?}");
    }
}

/// Windows: a same-process pair is the same user, so the pid gate
/// (`SIO_AF_UNIX_GETPEERPID` + token-user compare) passes it — and where
/// the ioctl exists it names this process: the negative control that the pid is the
/// peer's and not a stand-in.
#[test]
#[cfg(windows)]
fn peer_check_passes_a_same_user_peer_on_windows() {
    let (a, _b) = CtlStream::pair().expect("socketpair");
    assert_eq!(peer_check(&a), Ok(()));
    if let Ok(pid) = aterm_uds::fdpass::peer_pid(&a) {
        assert_eq!(pid, std::process::id());
    }
}

#[test]
fn sweep_removes_only_dead_instances_files() {
    let dir = std::env::temp_dir().join(format!("aterm-sweep-test-{}", std::process::id()));
    ensure_private_dir(&dir).unwrap();
    #[cfg(unix)]
    let mut child = aterm_uds::spawnfd::command("/bin/sleep")
        .arg("0")
        .spawn()
        .unwrap();
    #[cfg(windows)]
    let mut child = aterm_uds::spawnfd::command("cmd")
        .args(["/c", "exit"])
        .spawn()
        .unwrap();
    let dead = child.id();
    child.wait().unwrap();
    let us = std::process::id();
    assert!(pid_alive(us) && !pid_alive(dead));
    assert!(
        !pid_alive(0) && !pid_alive(u32::MAX),
        "a pid that cannot be real (0, wider than pid_t) is dead, never a group or broadcast"
    );
    #[cfg(unix)]
    {
        // SAFETY: getpgrp has no preconditions and cannot fail.
        let group = unsafe { libc::getpgrp() } as u32;
        assert!(!pid_alive(group.wrapping_neg()));
    }
    let touch = |name: &str| std::fs::write(dir.join(name), b"x").unwrap();
    touch(&control_socket::instance_sock_name(dead));
    touch(&control_socket::instance_token_name(dead));
    touch(&control_socket::instance_sock_name(us));
    touch(&control_socket::instance_token_name(us));
    touch(control_socket::SIBLING_TOKEN_FILE);
    touch(&control_socket::token_name_for_sock("private.sock"));

    sweep_stale_instances(&dir);

    assert!(!dir.join(control_socket::instance_sock_name(dead)).exists());
    assert!(!dir.join(control_socket::instance_token_name(dead)).exists());
    assert!(dir.join(control_socket::instance_sock_name(us)).exists());
    assert!(dir.join(control_socket::instance_token_name(us)).exists());
    assert!(dir.join(control_socket::SIBLING_TOKEN_FILE).exists());
    assert!(
        dir.join(control_socket::token_name_for_sock("private.sock"))
            .exists()
    );

    touch(&control_socket::instance_sock_name(dead));
    touch(&control_socket::instance_token_name(dead));
    let link = dir.join(SOCK_FILE);
    publish_latest_link(&link, &control_socket::instance_sock_name(dead));
    sweep_stale_instances(&dir);
    assert!(dir.join(control_socket::instance_sock_name(dead)).exists());
    assert!(dir.join(control_socket::instance_token_name(dead)).exists());
    publish_latest_link(&link, &control_socket::instance_sock_name(us));
    sweep_stale_instances(&dir);
    assert!(!dir.join(control_socket::instance_sock_name(dead)).exists());
    assert!(!dir.join(control_socket::instance_token_name(dead)).exists());
    assert!(dir.join(control_socket::instance_sock_name(us)).exists());
    let _ = std::fs::remove_dir_all(&dir);
}

/// The launch sweep removes only aged zero-byte socket files with no listener.
#[test]
fn launch_sweep_removes_only_aged_empty_socks_nobody_listens_on() {
    use std::time::{Duration, SystemTime};
    if !crate::test_alone::alone() {
        return;
    }
    let scratch = crate::test_scratch::short_dir("the swept sockets");
    let dir = scratch.path().join("socks");
    ensure_private_dir(&dir).unwrap();
    #[cfg(unix)]
    let mut child = aterm_uds::spawnfd::command("/bin/sleep")
        .arg("0")
        .spawn()
        .unwrap();
    #[cfg(windows)]
    let mut child = aterm_uds::spawnfd::command("cmd")
        .args(["/c", "exit"])
        .spawn()
        .unwrap();
    let dead = child.id();
    child.wait().unwrap();
    let present = |p: &Path| p.try_exists().unwrap_or(false);

    let two_days_ago =
        SystemTime::now() - Duration::from_secs(2 * control_socket::STALE_SOCK_MIN_AGE_SECS);
    let plant = |name: &str, body: &[u8], when: Option<SystemTime>| {
        let path = dir.join(name);
        std::fs::write(&path, body).unwrap();
        if let Some(when) = when {
            std::fs::File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_modified(when)
                .unwrap();
        }
    };
    let dead_sock = control_socket::instance_sock_name(dead);
    let dead_token = control_socket::instance_token_name(dead);
    plant(&dead_sock, b"", Some(two_days_ago));
    plant(&dead_token, b"tok", Some(two_days_ago));
    plant("young.sock", b"", None);
    plant("pointer.sock", b"aterm-1.sock", Some(two_days_ago));
    plant(control_socket::LATEST_SOCK_FILE, b"", Some(two_days_ago));
    let live_path = dir.join("live.sock");
    let live = aterm_uds::CtlListener::bind(&live_path).expect("bind live");
    let orphan_path = dir.join("orphan.sock");
    let orphan = aterm_uds::CtlListener::bind(&orphan_path).expect("bind orphan");
    drop(orphan);

    let report = sweep_stale_socket_files(&dir, STALE_SOCK_MIN_AGE, true).expect("dir listed");
    assert_eq!(report.removed, vec![dead_sock.clone()]);
    assert!(report.failed.is_empty());
    assert!(!present(&dir.join(&dead_sock)));
    assert!(present(&dir.join(&dead_token)));
    assert!(present(&dir.join("young.sock")));
    assert!(present(&dir.join("pointer.sock")));
    assert!(present(&dir.join(control_socket::LATEST_SOCK_FILE)));
    assert!(
        present(&live_path) && present(&orphan_path),
        "younger than the margin: neither real socket is probed or touched"
    );

    let orphan_name = orphan_path.to_string_lossy().into_owned();
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the dropped listener's socket refuses connections",
        || (!socket_is_live(&orphan_name)).then_some(()),
    );
    let report = sweep_stale_socket_files(&dir, Duration::ZERO, true).expect("dir listed");
    assert!(
        !report.removed.iter().any(|n| n == "live.sock"),
        "a live listener is never swept"
    );
    assert!(
        report.removed.iter().any(|n| n == "orphan.sock"),
        "a dropped listener's leftover must go once stale"
    );
    assert!(!present(&orphan_path));
    assert!(present(&live_path));
    assert!(
        socket_is_live(&live_path.to_string_lossy()),
        "still answering"
    );
    assert!(present(&dir.join(&dead_token)));
    assert!(present(&dir.join("pointer.sock")));
    assert!(present(&dir.join(control_socket::LATEST_SOCK_FILE)));

    plant(&dead_sock, b"", Some(two_days_ago));
    std::fs::remove_file(dir.join(control_socket::LATEST_SOCK_FILE)).unwrap();
    publish_latest_link(&dir.join(SOCK_FILE), &dead_sock);
    let report = sweep_stale_socket_files(&dir, STALE_SOCK_MIN_AGE, true).expect("dir listed");
    assert!(report.removed.is_empty(), "{report:?}");
    assert!(present(&dir.join(&dead_sock)));
    let report = sweep_stale_socket_files(&dir, STALE_SOCK_MIN_AGE, false).expect("dir listed");
    assert_eq!(report.removed, vec![dead_sock.clone()]);
    assert!(!present(&dir.join(&dead_sock)));

    assert!(sweep_stale_socket_files(&dir.join("absent"), Duration::ZERO, true).is_none());
    drop(live);
}

#[test]
fn latest_link_publishes_atomically_and_repoints() {
    let dir = std::env::temp_dir().join(format!("aterm-link-test-{}", std::process::id()));
    ensure_private_dir(&dir).unwrap();
    let link = dir.join(SOCK_FILE);

    let first = dir.join("aterm-101.sock");
    publish_latest_link(&link, first.to_str().unwrap());
    assert_eq!(
        aterm_uds::latest::target_name(&link).as_deref(),
        Some(std::ffi::OsStr::new("aterm-101.sock"))
    );

    let second = dir.join("aterm-202.sock");
    publish_latest_link(&link, second.to_str().unwrap());
    assert_eq!(
        aterm_uds::latest::target_name(&link).as_deref(),
        Some(std::ffi::OsStr::new("aterm-202.sock"))
    );
    assert!(!dir.join("aterm-202.sock.lnk").exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cleanup_removes_own_files_and_only_our_symlink() {
    let dir = std::env::temp_dir().join(format!("aterm-clean-test-{}", std::process::id()));
    ensure_private_dir(&dir).unwrap();
    let link = dir.join(SOCK_FILE);
    let plan = SocketPlan {
        sock_path: dir.join("aterm-4242.sock").to_string_lossy().into_owned(),
        token_path: dir.join("aterm-4242.token"),
        latest_link: Some(link.clone()),
    };
    let provision = || {
        std::fs::write(&plan.sock_path, b"x").unwrap();
        std::fs::write(&plan.token_path, b"x").unwrap();
    };

    provision();
    publish_latest_link(&link, &plan.sock_path);
    cleanup_socket(&plan);
    assert!(!Path::new(&plan.sock_path).exists());
    assert!(!plan.token_path.exists());
    assert!(aterm_uds::latest::target_name(&link).is_none());

    provision();
    publish_latest_link(&link, dir.join("aterm-9.sock").to_str().unwrap());
    cleanup_socket(&plan);
    assert!(!Path::new(&plan.sock_path).exists());
    assert_eq!(
        aterm_uds::latest::target_name(&link).as_deref(),
        Some(std::ffi::OsStr::new("aterm-9.sock"))
    );
    let _ = std::fs::remove_dir_all(&dir);
}

fn write_completed_video_fixture(root: &Path, name: &str) {
    let recording = root.join(name);
    std::fs::create_dir(&recording).unwrap();
    std::fs::write(recording.join("index.json"), b"{\"frames\":[]}").unwrap();
    std::fs::write(recording.join(VIDEO_PUBLISHED_FILE), b"published").unwrap();
}

fn completed_video_fixture_count(root: &Path) -> usize {
    std::fs::read_dir(root)
        .unwrap()
        .flatten()
        .filter(|entry| {
            entry.path().join("index.json").is_file()
                && entry.path().join(VIDEO_PUBLISHED_FILE).is_file()
        })
        .count()
}

/// Holds a path immutable (`chflags uchg`, as any same-uid actor can), so
/// no unlink or rename of it succeeds until this drops, unwinding included.
#[cfg(target_os = "macos")]
struct Immutable(std::ffi::CString);

#[cfg(target_os = "macos")]
impl Immutable {
    fn set(path: &Path) -> Self {
        use std::os::unix::ffi::OsStrExt as _;
        let path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: `path` is NUL-terminated and outlives the call.
        let set = unsafe { libc::chflags(path.as_ptr(), libc::UF_IMMUTABLE) };
        assert_eq!(set, 0, "{path:?}");
        Self(path)
    }
}

#[cfg(target_os = "macos")]
impl Drop for Immutable {
    fn drop(&mut self) {
        // SAFETY: `self.0` is NUL-terminated and outlives the call.
        unsafe { libc::chflags(self.0.as_ptr(), 0) };
    }
}

/// Arm `window` on this thread: the next pass through it runs `swap`, once.
#[cfg(unix)]
fn race_at(window: RaceWindow, swap: impl FnOnce() + 'static) {
    ARMED_RACE.set(Some((window, Box::new(swap))));
}

/// Whether the swap [`race_at`] armed has run; disarms it either way.
#[cfg(unix)]
fn race_ran() -> bool {
    ARMED_RACE.take().is_none()
}

fn artifact_lease_count(path: &Path) -> Option<usize> {
    let (leases, _) = artifact_path_leases();
    let held = leases
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    held.get(path).map(|state| state.count)
}

fn wait_for_artifact_cleanup(path: &Path) {
    wait_for_artifact_cleanup_for_test(path);
}

#[test]
fn video_producer_lease_spans_marker_visible_reader_handoff() {
    let dir = std::env::temp_dir().join(format!(
        "aterm-video-marker-reader-handoff-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    ensure_private_dir(&dir).unwrap();
    let instance = format!("p{}-marker-reader", std::process::id());

    let mut producer = confine_video_dir_for_instance(&dir, &instance).unwrap();
    let path = producer.path().to_path_buf();
    let flushes = aterm_durable::seam::Recording::start();
    let index = producer
        .write_new_private(std::ffi::OsStr::new("index.json"), b"{\"frames\":[]}")
        .unwrap();
    assert_eq!(
        flushes.take(),
        [aterm_durable::Flush::Ordered],
        "the index's bytes"
    );
    producer.publish(&[], &index).unwrap();
    assert_eq!(
        flushes.take(),
        [],
        "the batch barrier: the directory's plain flush"
    );
    assert_eq!(
        artifact_lease_count(&path),
        Some(1),
        "the producer owns the sole pre-publication lease"
    );
    assert!(
        !path.join(VIDEO_PUBLISHED_FILE).exists(),
        "the index is still invisible to readers before marker publication"
    );

    let marker = producer.publish_marker().unwrap();
    assert_eq!(
        flushes.take(),
        [aterm_durable::Flush::Ordered],
        "the marker's bytes"
    );
    drop(flushes);
    assert!(path.join(VIDEO_PUBLISHED_FILE).is_file());
    assert_eq!(
        artifact_lease_count(&path),
        Some(1),
        "publishing the marker does not release the producer lease"
    );

    let root = crate::pinned_dir::PinnedDir::open_resolved(path.parent().unwrap()).unwrap();
    let name = path.file_name().expect("recording name").to_os_string();
    let reader_recording = root.child(&name).unwrap();
    let reader = retain_video_artifact_path(
        root,
        name,
        &reader_recording,
        &crate::control::ArtifactBudgets::shipping(),
    )
    .unwrap()
    .expect("marker-visible reader lease");
    reader.arm_video_retention_sweep(&reader_recording).unwrap();
    drop(reader_recording);
    assert_eq!(
        artifact_lease_count(&path),
        Some(2),
        "reader entry overlaps the still-live producer lease"
    );

    drop(marker);
    drop(index);
    drop(producer);
    assert_eq!(
        artifact_lease_count(&path),
        Some(1),
        "producer release hands ownership directly to the reader"
    );
    assert!(
        path.is_dir(),
        "the marker-visible reader still owns the path"
    );

    drop(reader);
    wait_for_artifact_cleanup(&path);
    assert_eq!(
        artifact_lease_count(&path),
        None,
        "the final reader release completes the 2→1→0 handoff"
    );
    assert!(
        path.is_dir(),
        "reader-release retention preserves the just-advertised recording"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn video_producer_sweeps_from_its_local_root_when_reader_releases_first() {
    let dir = std::env::temp_dir().join(format!(
        "aterm-video-producer-final-sweep-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    ensure_private_dir(&dir).unwrap();
    let instance = format!("p{}-producer-final", std::process::id());

    let mut producer = confine_video_dir_for_instance(&dir, &instance).unwrap();
    let path = producer.path().to_path_buf();
    let root_path = path.parent().unwrap().to_path_buf();
    let index = producer
        .write_new_private(std::ffi::OsStr::new("index.json"), b"{\"frames\":[]}")
        .unwrap();
    producer.publish(&[], &index).unwrap();
    let marker = producer.publish_marker().unwrap();

    let oldest = root_path.join("rec-00000000000000000000-100");
    for sequence in 100..112 {
        write_completed_video_fixture(
            &root_path,
            &format!("rec-00000000000000000000-{sequence:03}"),
        );
    }

    let reader_root = crate::pinned_dir::PinnedDir::open_resolved(&root_path).unwrap();
    let name = path.file_name().unwrap().to_os_string();
    let reader_recording = reader_root.child(&name).unwrap();
    let reader = retain_video_artifact_path(
        reader_root,
        name,
        &reader_recording,
        &crate::control::ArtifactBudgets::shipping(),
    )
    .unwrap()
    .expect("reader overlaps the published producer");
    drop(reader_recording);
    drop(reader);
    assert!(
        oldest.is_dir(),
        "a nonfinal reader release cannot run retention"
    );

    drop(marker);
    drop(index);
    drop(producer);
    wait_for_artifact_cleanup(&path);
    assert!(
        !oldest.exists(),
        "the final producer retains its own charged root through the sweep"
    );
    assert!(path.is_dir(), "the fresh published recording is retained");

    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn unarmed_final_reader_inherits_sweep_request_on_its_local_root() {
    let dir = std::env::temp_dir().join(format!(
        "aterm-video-unarmed-final-reader-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    ensure_private_dir(&dir).unwrap();
    let root_path = dir.join("recordings");
    ensure_private_dir(&root_path).unwrap();
    for sequence in 0..12 {
        write_completed_video_fixture(&root_path, &format!("rec-{sequence:020}-000"));
    }
    let fresh = std::ffi::OsString::from("rec-00000000000000000011-000");

    let first_root = crate::pinned_dir::PinnedDir::open_resolved(&root_path).unwrap();
    let first_recording = first_root.child(&fresh).unwrap();
    let first = retain_video_artifact_path(
        first_root,
        fresh.clone(),
        &first_recording,
        &crate::control::ArtifactBudgets::shipping(),
    )
    .unwrap()
    .expect("first reader lease");
    let key = first.key.clone().expect("reader owns its canonical key");
    first.arm_video_retention_sweep(&first_recording).unwrap();
    drop(first_recording);

    let second_root = crate::pinned_dir::PinnedDir::open_resolved(&root_path).unwrap();
    let second_recording = second_root.child(&fresh).unwrap();
    let second = retain_video_artifact_path(
        second_root,
        fresh,
        &second_recording,
        &crate::control::ArtifactBudgets::shipping(),
    )
    .unwrap()
    .expect("overlapping reader lease");
    drop(second_recording);

    drop(first);
    assert_eq!(artifact_lease_count(&key), Some(1));
    assert_eq!(
        completed_video_fixture_count(&root_path),
        12,
        "the first release cannot sweep through a live second reader"
    );

    let moved = dir.join("recordings-original");
    std::fs::rename(&root_path, &moved).unwrap();
    ensure_private_dir(&root_path).unwrap();
    for sequence in 100..112 {
        write_completed_video_fixture(&root_path, &format!("rec-{sequence:020}-000"));
    }
    std::fs::write(root_path.join("sentinel"), b"replacement").unwrap();

    drop(second);
    wait_for_artifact_cleanup(&key);
    assert!(
        completed_video_fixture_count(&moved) < 12,
        "the unarmed final reader honors the prior sweep request on its own root"
    );
    assert_eq!(
        completed_video_fixture_count(&root_path),
        12,
        "the replacement at the old lexical path is untouched"
    );
    assert_eq!(
        std::fs::read(root_path.join("sentinel")).unwrap(),
        b"replacement"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn video_lease_capabilities_bind_their_own_recording_and_root() {
    let dir = std::env::temp_dir().join(format!(
        "aterm-video-reader-root-identity-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    ensure_private_dir(&dir).unwrap();
    let root_path = dir.join("recordings");
    ensure_private_dir(&root_path).unwrap();
    let fresh = std::ffi::OsString::from("rec-00000000000000000000-000");
    write_completed_video_fixture(&root_path, fresh.to_str().unwrap());

    let other = std::ffi::OsString::from("rec-00000000000000000001-000");
    write_completed_video_fixture(&root_path, other.to_str().unwrap());

    let first_root = crate::pinned_dir::PinnedDir::open_resolved(&root_path).unwrap();
    let first_recording = first_root.child(&fresh).unwrap();
    let other_recording = first_root.child(&other).unwrap();
    let first = retain_video_artifact_path(
        first_root,
        fresh.clone(),
        &first_recording,
        &crate::control::ArtifactBudgets::shipping(),
    )
    .unwrap()
    .expect("first reader lease");

    let again = crate::pinned_dir::PinnedDir::open_resolved(&root_path).unwrap();
    let invalid = |result: std::io::Result<()>| result.unwrap_err().kind();
    assert_eq!(
        invalid(
            retain_video_artifact_path(
                again,
                fresh.clone(),
                &other_recording,
                &crate::control::ArtifactBudgets::shipping(),
            )
            .map(drop)
        ),
        std::io::ErrorKind::InvalidInput
    );
    assert_eq!(
        invalid(first.arm_video_retention_sweep(&other_recording)),
        std::io::ErrorKind::InvalidInput
    );
    let aside = root_path.join("rec-aside");
    std::fs::rename(root_path.join(&fresh), &aside).unwrap();
    std::fs::create_dir(root_path.join(&fresh)).unwrap();
    let impostor = crate::pinned_dir::PinnedDir::open_resolved(&root_path)
        .unwrap()
        .child(&fresh)
        .unwrap();
    for handle in [&impostor, &first_recording] {
        assert_eq!(
            invalid(first.arm_video_retention_sweep(handle)),
            std::io::ErrorKind::NotFound
        );
    }
    drop(impostor);
    std::fs::remove_dir(root_path.join(&fresh)).unwrap();
    std::fs::rename(&aside, root_path.join(&fresh)).unwrap();
    drop((first_recording, other_recording));

    let original_root = dir.join("recordings-original");
    std::fs::rename(&root_path, &original_root).unwrap();
    ensure_private_dir(&root_path).unwrap();
    std::fs::rename(original_root.join(&fresh), root_path.join(&fresh)).unwrap();

    let replacement_root = crate::pinned_dir::PinnedDir::open_resolved(&root_path).unwrap();
    let same_recording = replacement_root.child(&fresh).unwrap();
    let error = retain_video_artifact_path(
        replacement_root,
        fresh,
        &same_recording,
        &crate::control::ArtifactBudgets::shipping(),
    )
    .expect_err("the same recording inode cannot cross into a replacement root");
    assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
    assert_eq!(artifact_lease_count(first.key.as_ref().unwrap()), Some(1));
    assert_eq!(
        invalid(first.arm_video_retention_sweep(&same_recording)),
        std::io::ErrorKind::NotFound
    );

    drop(same_recording);
    drop(first);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn video_dir_prune_keeps_the_newest_recordings() {
    let dir = std::env::temp_dir().join(format!("aterm-vid-prune-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    ensure_private_dir(&dir).unwrap();
    let instance = format!("p{}-prune", std::process::id());
    let bootstrap = super::confine_video_dir_for_instance(&dir, &instance)
        .expect("create leased instance namespace");
    let root = bootstrap
        .path()
        .parent()
        .expect("recording has instance parent")
        .to_path_buf();
    drop(bootstrap);

    for i in 0..12 {
        let d = root.join(format!("rec-{i:020}-000"));
        std::fs::create_dir(&d).unwrap();
        std::fs::write(d.join("index.json"), b"{\"frames\":[]}").unwrap();
        std::fs::write(d.join(VIDEO_PUBLISHED_FILE), b"published").unwrap();
    }
    let in_flight = root.join("rec-00000000000000000000-999");
    std::fs::create_dir(&in_flight).unwrap();

    let abandoned =
        super::confine_video_dir_for_instance(&dir, &instance).expect("mint unpublished recording");
    assert_eq!(
        (0..12)
            .filter(|i| root.join(format!("rec-{i:020}-000")).is_dir())
            .count(),
        12,
        "mint alone never prunes completed recordings"
    );
    drop(abandoned);

    let mut fresh =
        super::confine_video_dir_for_instance(&dir, &instance).expect("mint recording to publish");
    let fresh_path = fresh.path().to_path_buf();
    let index = fresh
        .write_new_private(std::ffi::OsStr::new("index.json"), b"{\"frames\":[]}")
        .unwrap();
    let published = fresh.publish(&[], &index).expect("publish durable batch");
    let marker = fresh.publish_marker().unwrap();
    drop(marker);
    drop(index);
    drop(fresh);
    wait_for_artifact_cleanup(&fresh_path);
    assert_eq!(published, fresh_path);
    let mut recs: Vec<String> = std::fs::read_dir(&root)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("rec-"))
        .collect();
    recs.sort();
    assert!(
        in_flight.is_dir(),
        "an in-flight (index-less) recording is never pruned mid-encode"
    );
    assert_eq!(
        recs.len(),
        super::VIDEO_KEEP + 1,
        "prune keeps VIDEO_KEEP COMPLETED recordings (incl. fresh) + the in-flight one"
    );
    assert!(
        published.is_dir(),
        "the just-published recording survives retention"
    );
    for i in 0..5 {
        assert!(
            !root.join(format!("rec-{i:020}-000")).exists(),
            "old completed recording {i} is pruned"
        );
    }
    for i in 5..12 {
        assert!(
            root.join(format!("rec-{i:020}-000")).is_dir(),
            "newer completed recording {i} survives"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn video_retention_preserves_every_queued_wire_reply() {
    let dir = std::env::temp_dir().join(format!("aterm-vid-queued-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    ensure_private_dir(&dir).unwrap();
    let instance = format!("p{}-queued", std::process::id());

    let mut queued = confine_video_dir_for_instance(&dir, &instance).unwrap();
    let queued_path = queued.path().to_path_buf();
    let queued_index = queued
        .write_new_private(std::ffi::OsStr::new("index.json"), b"{\"frames\":[]}")
        .unwrap();
    queued.publish(&[], &queued_index).unwrap();
    drop(queued_index);
    let root = queued_path.parent().unwrap().to_path_buf();

    for sequence in 0..(VIDEO_KEEP + 3) {
        let name = format!("rec-9999999999999999{sequence:04}-000");
        let recording = root.join(name);
        std::fs::create_dir(&recording).unwrap();
        std::fs::write(recording.join("index.json"), b"{\"frames\":[]}").unwrap();
        std::fs::write(recording.join(VIDEO_PUBLISHED_FILE), b"published").unwrap();
    }

    let mut fresh = confine_video_dir_for_instance(&dir, &instance).unwrap();
    let fresh_path = fresh.path().to_path_buf();
    let fresh_index = fresh
        .write_new_private(std::ffi::OsStr::new("index.json"), b"{\"frames\":[]}")
        .unwrap();
    fresh.publish(&[], &fresh_index).unwrap();
    let fresh_marker = fresh.publish_marker().unwrap();
    drop(fresh_marker);
    drop(fresh_index);
    drop(fresh);
    wait_for_artifact_cleanup(&fresh_path);

    assert!(
        queued_path.join("index.json").is_file(),
        "a later recording's sweep cannot delete an older queued video reply"
    );
    assert!(
        !queued_path.join(VIDEO_PUBLISHED_FILE).exists(),
        "the queued fixture stays pre-wire: its publication marker is absent"
    );
    assert!(fresh_path.join("index.json").is_file());
    let completed = std::fs::read_dir(&root)
        .unwrap()
        .flatten()
        .filter(|entry| {
            entry.path().join("index.json").is_file()
                && entry.path().join(VIDEO_PUBLISHED_FILE).is_file()
        })
        .count();
    assert_eq!(
        completed, VIDEO_KEEP,
        "queued video replies consume retention slots without exceeding the cap"
    );

    drop(queued);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn video_prune_has_one_deterministic_global_work_counter() {
    let root = std::env::temp_dir().join(format!(
        "aterm-video-prune-global-budget-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    for recording in 0..9 {
        let dir = root.join(format!("rec-{recording:020}-000"));
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("index.json"), b"{\"frames\":[]}").unwrap();
        std::fs::write(dir.join(VIDEO_PUBLISHED_FILE), b"published").unwrap();
    }
    let fresh_name = std::ffi::OsString::from("rec-99999999999999999999-000");
    let fresh = root.join(&fresh_name);
    std::fs::create_dir(&fresh).unwrap();
    std::fs::write(fresh.join("index.json"), b"{\"frames\":[]}").unwrap();
    std::fs::write(fresh.join(VIDEO_PUBLISHED_FILE), b"published").unwrap();
    let pinned = crate::pinned_dir::PinnedDir::open_resolved(&root).unwrap();

    let limit = 10 * VIDEO_SCAN_WORK + 19 * VIDEO_FILE_OPEN_WORK + (VIDEO_FILE_OPEN_WORK - 1);
    let mut work = VideoPruneWork::new(limit);
    prune_video_dirs_with_work(&pinned, &fresh_name, &mut work).unwrap();

    assert_eq!(work.scanned, 10);
    assert_eq!(work.file_opens, 19);
    assert_eq!(work.renames, 0);
    assert_eq!(work.deletes, 0);
    assert!(work.remaining < VIDEO_FILE_OPEN_WORK);
    assert!(
        fresh.join("index.json").is_file(),
        "budget exhaustion cannot affect the freshly published recording"
    );

    let limit = 10 * VIDEO_SCAN_WORK + 20 * VIDEO_FILE_OPEN_WORK + (VIDEO_DELETE_WORK - 1);
    let mut work = VideoPruneWork::new(limit);
    prune_video_dirs_with_work(&pinned, &fresh_name, &mut work).unwrap();
    assert_eq!((work.file_opens, work.renames), (20, 0));
    assert!(root.join(format!("rec-{:020}-000", 0)).is_dir());
    let mut exact = VideoPruneWork::new(VIDEO_SCAN_WORK);
    assert!(exact.scan() && !exact.scan());

    #[cfg(target_os = "macos")]
    {
        let stuck = std::ffi::OsString::from(".prune-stuck");
        std::fs::create_dir(root.join(&stuck)).unwrap();
        std::fs::write(root.join(&stuck).join("frame"), b"png").unwrap();
        let undeletable = Immutable::set(&root.join(&stuck).join("frame"));
        let tombstone = pinned.child(&stuck).unwrap();
        let mut work = VideoPruneWork::new(VIDEO_PRUNE_WORK_LIMIT);
        let cleanup = cleanup_video_tombstone(&pinned, &stuck, &tombstone, &mut work);
        assert!(!cleanup.removed);
        assert_eq!(work.deletes, 1);
        drop((undeletable, tombstone));
        std::fs::remove_dir_all(root.join(&stuck)).unwrap();
    }

    let held = std::ffi::OsString::from(format!("rec-{:020}-000", 0));
    let lease_root = crate::pinned_dir::PinnedDir::open_resolved(&root).unwrap();
    let held_recording = lease_root.child(&held).unwrap();
    let lease = retain_video_artifact_path(
        lease_root,
        held.clone(),
        &held_recording,
        &crate::control::ArtifactBudgets::shipping(),
    )
    .unwrap()
    .expect("reader lease");
    let mut work = VideoPruneWork::new(VIDEO_PRUNE_WORK_LIMIT);
    prune_video_dirs_with_work(&pinned, &fresh_name, &mut work).unwrap();
    drop((lease, held_recording));
    assert_eq!(work.renames, 2);
    assert!(root.join(&held).is_dir());
    for taken in 1..=2 {
        assert!(!root.join(format!("rec-{taken:020}-000")).exists());
    }

    let _ = std::fs::remove_dir_all(&root);
}

/// Automatic-image namespace GC owns another retained directory chain.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn automatic_image_gc_waits_for_final_capability_under_low_nofile() {
    if !enter_low_nofile_test_child(16) {
        return;
    }

    let temp = crate::test_scratch::short_dir("the automatic image root");
    let target = confine_automatic_image_path(temp.path(), "image")
        .expect("confinement fits the descriptor budget");
    let namespace_root = target.dir.parent().unwrap().to_path_buf();
    let path = target.display_path();
    let lease = acquire_capture_name_lease(&target, || false)
        .unwrap()
        .expect("automatic path lease");
    let file = target
        .write_private(b"png")
        .expect("no concurrent GC consumes the remaining file descriptor");
    target.validate_for_reply(&file).unwrap();
    for sequence in 0..(AUTO_IMAGE_KEEP + 2) {
        let name =
            automatic_capture_name_for("image", process_instance_id(), 9_000_000 + sequence as u64);
        std::fs::write(target.dir.join(name), b"old").unwrap();
    }
    target.arm_automatic_prune();
    assert!(path.is_file());
    assert!(
        !pending_dead_namespace_sweeps()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains_key(&namespace_root),
        "automatic-image confinement must not eagerly schedule namespace GC"
    );

    drop(target);
    drop(file);
    let image_count = || {
        std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .flatten()
            .filter(|entry| automatic_capture_sequence(&entry.file_name()).is_some())
            .count()
    };
    assert!(
        image_count() > AUTO_IMAGE_KEEP,
        "auto-image retention must remain deferred while the path lease is live"
    );
    assert!(
        !pending_dead_namespace_sweeps()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains_key(&namespace_root),
        "the path lease keeps cleanup deferred until every capability closes"
    );
    drop(lease);
    wait_for_best_effort_cleanup_barrier_for_test();
    assert_eq!(
        image_count(),
        AUTO_IMAGE_KEEP,
        "the pre-reserved worker sweep converges after the final capability closes"
    );
}

/// Scanning keeps names, not one open directory per recording.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn video_prune_directory_handles_stay_bounded() {
    if !enter_low_nofile_test_child(16) {
        return;
    }

    let temp = crate::test_scratch::short_dir("the video root");
    let root = temp.path();
    for sequence in 0..32 {
        write_completed_video_fixture(root, &format!("rec-{sequence:020}-000"));
    }
    let fresh = std::ffi::OsString::from("rec-99999999999999999999-000");
    write_completed_video_fixture(root, fresh.to_str().unwrap());
    let pinned = crate::pinned_dir::PinnedDir::open_resolved(root).unwrap();

    prune_video_dirs(&pinned, &fresh).unwrap();
    assert_eq!(completed_video_fixture_count(root), VIDEO_KEEP);
    assert!(root.join(&fresh).is_dir());
}

#[test]
fn video_prune_treats_corrupt_and_oversized_indexes_as_completed() {
    let temp = aterm_tempfile::tempdir().expect("temporary video root");
    let root = temp.path();
    for sequence in 0..(VIDEO_KEEP + 2) {
        write_completed_video_fixture(root, &format!("rec-{sequence:020}-000"));
    }
    let corrupt = root.join("rec-00000000000000000000-000");
    std::fs::write(corrupt.join("index.json"), b"not json").unwrap();
    let oversized = root.join("rec-00000000000000000001-000");
    std::fs::OpenOptions::new()
        .write(true)
        .open(oversized.join("index.json"))
        .unwrap()
        .set_len(32 * 1024 * 1024)
        .unwrap();

    write_completed_video_fixture(root, "operator-kept");
    let marker_only = root.join("rec-00000000000000000000-001");
    std::fs::create_dir(&marker_only).unwrap();
    std::fs::write(marker_only.join(VIDEO_PUBLISHED_FILE), b"published").unwrap();
    let linked_marker = root.join("rec-00000000000000000000-002");
    std::fs::create_dir(&linked_marker).unwrap();
    std::fs::write(linked_marker.join("index.json"), b"{\"frames\":[]}").unwrap();
    std::fs::write(root.join("marker-twin"), b"twin").unwrap();
    std::fs::hard_link(
        root.join("marker-twin"),
        linked_marker.join(VIDEO_PUBLISHED_FILE),
    )
    .unwrap();
    let elsewhere = aterm_tempfile::tempdir().expect("temporary second root");
    let same_name = std::ffi::OsString::from("rec-00000000000000000002-000");
    write_completed_video_fixture(elsewhere.path(), same_name.to_str().unwrap());
    let elsewhere_root = crate::pinned_dir::PinnedDir::open_resolved(elsewhere.path()).unwrap();
    let elsewhere_recording = elsewhere_root.child(&same_name).unwrap();
    let elsewhere_lease = retain_video_artifact_path(
        elsewhere_root,
        same_name.clone(),
        &elsewhere_recording,
        &crate::control::ArtifactBudgets::shipping(),
    )
    .unwrap()
    .expect("reader lease elsewhere");
    let crashed = root.join(".prune-crashed");
    std::fs::create_dir(&crashed).unwrap();
    std::fs::write(crashed.join("frame-000000.png"), b"png").unwrap();
    #[cfg(target_os = "macos")]
    let unrenamable = root.join("rec-00000000000000000003-000");
    #[cfg(target_os = "macos")]
    let _unrenamable = Immutable::set(&unrenamable);

    let fresh = std::ffi::OsString::from("rec-99999999999999999999-000");
    write_completed_video_fixture(root, fresh.to_str().unwrap());
    let pinned = crate::pinned_dir::PinnedDir::open_resolved(root).unwrap();
    prune_video_dirs(&pinned, &fresh).unwrap();
    drop((elsewhere_lease, elsewhere_recording));
    assert!(root.join("operator-kept").is_dir() && marker_only.is_dir());
    assert!(linked_marker.is_dir() && !root.join(&same_name).exists());
    assert!(!crashed.exists());
    #[cfg(target_os = "macos")]
    assert!(unrenamable.is_dir() && !root.join("rec-00000000000000000004-000").exists());

    assert!(
        !corrupt.exists(),
        "retention completion is the marker/index identity pair, not JSON parsing"
    );
    assert!(
        !oversized.exists(),
        "retention never reads an attacker-sized completed index"
    );
    // The hard-linked marker's recording is kept (its marker is refused
    // when pruned). Unix's census counts it among the completed, so it
    // holds a slot and one more old recording goes; Windows' census
    // refuses a multiply-linked file at once, so it holds none.
    assert_eq!(
        completed_video_fixture_count(root),
        VIDEO_KEEP + if cfg!(windows) { 2 } else { 1 },
        "{:?}",
        std::fs::read_dir(root)
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name())
            .collect::<Vec<_>>()
    );
    assert!(root.join(fresh).is_dir());
}

#[test]
fn video_member_write_after_sync_invalidates_marker_guard() {
    let temp = aterm_tempfile::tempdir().expect("temporary video root");
    let instance = format!("p{}-batch-state", std::process::id());
    let mut recording = confine_video_dir_for_instance(temp.path(), &instance).unwrap();
    let path = recording.path().to_path_buf();
    let index = recording
        .write_batch_member_authorized(
            std::ffi::OsStr::new("index.json"),
            b"{\"frames\":[]}",
            || true,
        )
        .unwrap();
    assert!(!recording.batch_synced_for_test());
    recording.publish(&[], &index).unwrap();
    assert!(recording.batch_synced_for_test());

    let late_frame = recording
        .write_sealed_frame(std::ffi::OsStr::new("frame_0001.png"), b"png")
        .unwrap();
    assert!(!recording.batch_synced_for_test());
    let error = recording
        .publish_marker()
        .expect_err("a later member invalidates the earlier directory barrier");
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    assert!(!path.join(VIDEO_PUBLISHED_FILE).exists());

    recording.publish(&[late_frame], &index).unwrap();
    assert!(recording.batch_synced_for_test());
    drop(recording.publish_marker().unwrap());
    assert!(path.join(VIDEO_PUBLISHED_FILE).is_file());

    #[cfg(unix)]
    {
        let mut raced = confine_video_dir_for_instance(temp.path(), &instance).unwrap();
        let raced_path = raced.path().to_path_buf();
        let index = raced
            .write_batch_member_authorized(
                std::ffi::OsStr::new("index.json"),
                b"{\"frames\":[]}",
                || true,
            )
            .unwrap();
        raced.publish(&[], &index).unwrap();
        let namespace = raced_path.parent().unwrap().to_path_buf();
        let aside = namespace.with_extension("aside");
        race_at(RaceWindow::MarkerVisible, {
            let (namespace, aside) = (namespace.clone(), aside.clone());
            move || std::fs::rename(&namespace, &aside).unwrap()
        });
        let published = raced.publish_marker_with_test_hook(|| {
            std::fs::rename(&aside, &namespace).unwrap();
        });
        assert!(race_ran());
        assert!(published.is_err());
        assert!(raced_path.join(VIDEO_PUBLISHED_FILE).is_file());
    }
}

#[test]
fn video_publish_rejects_replaced_frame_before_index_commit() {
    let dir = std::env::temp_dir().join(format!("aterm-video-frame-delete-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    ensure_private_dir(&dir).unwrap();
    let instance = format!("p{}-frame-delete", std::process::id());
    let mut recording = confine_video_dir_for_instance(&dir, &instance).unwrap();
    let path = recording.path().to_path_buf();
    let frame = recording
        .write_sealed_frame(std::ffi::OsStr::new("frame_0001.png"), b"png")
        .unwrap();
    std::fs::rename(path.join("frame_0001.png"), path.join("frame_original.png")).unwrap();
    drop(
        recording
            .write_new_private(std::ffi::OsStr::new("frame_0001.png"), b"replacement")
            .unwrap(),
    );
    let index = recording
        .write_new_private(
            std::ffi::OsStr::new("index.json"),
            br#"{"frames":[{"file":"frame_0001.png"}]}"#,
        )
        .unwrap();

    assert!(
        recording.publish(&[frame], &index).is_err(),
        "an index may never certify a same-name replacement"
    );
    drop(index);
    drop(recording);
    assert!(!path.exists(), "failed publication cleans the partial tree");
    let _ = std::fs::remove_dir_all(dir);
}

#[cfg(unix)]
#[test]
fn video_abort_removes_only_the_retained_instance_after_ancestor_swap() {
    use std::os::unix::fs::symlink;

    let dir = std::env::temp_dir().join(format!("aterm-video-abort-swap-{}", std::process::id()));
    let replacement = std::env::temp_dir().join(format!(
        "aterm-video-abort-replacement-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&replacement);
    ensure_private_dir(&dir).unwrap();
    ensure_private_dir(&replacement).unwrap();
    std::fs::write(replacement.join("keep.txt"), b"keep").unwrap();
    let instance = format!("p{}-abort-swap", std::process::id());
    let recording = confine_video_dir_for_instance(&dir, &instance).unwrap();
    let recording_name = recording.path().file_name().unwrap().to_os_string();
    let instance_path = recording.path().parent().unwrap().to_path_buf();
    let moved = instance_path.with_extension("moved");
    std::fs::rename(&instance_path, &moved).unwrap();
    symlink(&replacement, &instance_path).unwrap();

    recording.abort().unwrap();
    assert!(!moved.join(recording_name).exists());
    assert_eq!(
        std::fs::read(replacement.join("keep.txt")).unwrap(),
        b"keep"
    );

    let _ = std::fs::remove_file(instance_path);
    let _ = std::fs::remove_dir_all(dir);
    let _ = std::fs::remove_dir_all(replacement);
}

/// A child name is judged raw: `"x/"` cannot reach through the dangling link `x` and make
/// (and chmod) a directory outside `parent`.
#[test]
#[cfg(unix)]
fn a_direct_child_name_with_a_trailing_slash_makes_nothing_outside() {
    use std::os::unix::fs::symlink;
    let root =
        std::env::temp_dir().join(format!("aterm-direct-child-slash-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let parent = root.join("parent");
    let outside = root.join("outside");
    std::fs::create_dir_all(&parent).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    symlink(outside.join("absent"), parent.join("dangle")).unwrap();

    for name in ["dangle/", "dangle/.", "dangle//"] {
        let result = ensure_canonical_direct_child(&parent, name);
        assert!(result.is_err(), "{name:?} was accepted: {result:?}");
        assert!(
            !outside.join("absent").exists(),
            "{name:?} made a directory outside its parent"
        );
    }

    use std::os::unix::fs::PermissionsExt as _;
    let sibling = parent.join("sibling");
    std::fs::create_dir(&sibling).unwrap();
    std::fs::set_permissions(&sibling, std::fs::Permissions::from_mode(0o755)).unwrap();
    symlink(&sibling, parent.join("linked")).unwrap();
    std::fs::write(parent.join("file"), b"").unwrap();
    for name in ["linked", "file"] {
        assert_eq!(
            ensure_canonical_direct_child(&parent, name)
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::PermissionDenied,
            "{name}"
        );
    }
    let mode = |dir: &Path| std::fs::metadata(dir).unwrap().permissions().mode() & 0o7777;
    assert_eq!(mode(&sibling), 0o755);
    assert_eq!(
        ensure_canonical_direct_child(&parent, "sibling").unwrap(),
        std::fs::canonicalize(&sibling).unwrap()
    );
    assert_eq!(mode(&sibling), 0o700);

    let stranger = root.join("stranger");
    std::fs::create_dir(&stranger).unwrap();
    std::fs::set_permissions(&stranger, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::create_dir(parent.join("raced")).unwrap();
    race_at(RaceWindow::ChildLooked, {
        let (raced, stranger) = (parent.join("raced"), stranger.clone());
        move || {
            std::fs::remove_dir(&raced).unwrap();
            symlink(&stranger, &raced).unwrap();
        }
    });
    let raced = ensure_canonical_direct_child(&parent, "raced");
    assert!(race_ran());
    assert_eq!(
        raced.unwrap_err().kind(),
        std::io::ErrorKind::PermissionDenied
    );
    assert_eq!(mode(&stranger), 0o755);
    race_at(RaceWindow::ChildLooked, {
        let (planted, sibling) = (parent.join("planted"), sibling.clone());
        move || symlink(&sibling, &planted).unwrap()
    });
    let planted = ensure_canonical_direct_child(&parent, "planted");
    assert!(race_ran());
    assert_eq!(
        planted.unwrap_err().kind(),
        std::io::ErrorKind::PermissionDenied
    );
    let behind = root.join("parent.behind");
    race_at(RaceWindow::ChildLooked, {
        let (parent, behind) = (parent.clone(), behind.clone());
        move || {
            std::fs::rename(&parent, &behind).unwrap();
            symlink(&behind, &parent).unwrap();
        }
    });
    let moved = ensure_canonical_direct_child(&parent, "moved");
    assert!(race_ran());
    assert_eq!(
        moved.unwrap_err().kind(),
        std::io::ErrorKind::PermissionDenied
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn instance_lease_beats_pid_reuse_and_pid_false_negative() {
    let root = std::env::temp_dir().join(format!("aterm-instance-lease-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    ensure_private_dir(&root).unwrap();

    let reused = ensure_canonical_direct_child(&root, "p424242-reused").unwrap();
    let lease = HeldAdvisoryLock::try_lock(open_instance_lease(&reused, true).unwrap()).unwrap();
    drop(lease);
    sweep_dead_instance_namespaces_with(&root, "p1-current", |_| true);
    assert!(
        !reused.exists(),
        "an unlocked exact-instance lease wins over a reused live PID"
    );

    let held = ensure_canonical_direct_child(&root, "p424243-held").unwrap();
    let lease = HeldAdvisoryLock::try_lock(open_instance_lease(&held, true).unwrap()).unwrap();
    sweep_dead_instance_namespaces_with(&root, "p1-current", |_| false);
    assert!(
        held.is_dir(),
        "a contended exact-instance lease is never swept"
    );
    drop(lease);

    let current = ensure_canonical_direct_child(&root, "p424240-current").unwrap();
    let unnamed = ["not-an-instance", "424241-unprefixed", "p424242"]
        .map(|name| ensure_canonical_direct_child(&root, name).unwrap());
    let tombstone = root.join(format!("{DEAD_INSTANCE_TOMBSTONE_PREFIX}left"));
    std::fs::create_dir(&tombstone).unwrap();
    std::fs::write(tombstone.join(INSTANCE_LEASE_FILE), b"").unwrap();
    sweep_dead_instance_namespaces_with(&root, "p424240-current", |_| false);
    assert!(current.is_dir() && unnamed.iter().all(|dir| dir.is_dir()));
    assert!(!tombstone.exists());

    std::fs::create_dir(&tombstone).unwrap();
    let pinned_root = crate::pinned_dir::PinnedDir::open_resolved(&root).unwrap();
    let name = tombstone.file_name().unwrap();
    let pinned_tombstone = pinned_root.child(name).unwrap();
    let mut budget = 0;
    assert!(!cleanup_instance_tombstone(
        &pinned_root,
        name,
        &pinned_tombstone,
        &mut budget
    ));
    assert!(tombstone.is_dir());
    let mut budget = 1;
    assert!(cleanup_instance_tombstone(
        &pinned_root,
        name,
        &pinned_tombstone,
        &mut budget
    ));
    assert_eq!(budget, 0);
    // Windows deletes a directory once its last handle closes.
    drop((pinned_tombstone, pinned_root));
    assert!(!tombstone.exists());

    let ours = format!("p{}-leased", std::process::id());
    drop(confine_video_dir_for_instance(&root, &ours).unwrap());
    sweep_dead_instance_namespaces_with(&root.join(VIDEO_DIR), "p1-current", |_| false);
    assert!(root.join(VIDEO_DIR).join(&ours).is_dir());
    let _ = std::fs::remove_dir_all(&root);
}

#[cfg(unix)]
#[test]
fn instance_lease_authority_stays_on_pinned_root_after_ancestor_swap() {
    let temp = aterm_tempfile::tempdir().expect("temporary namespace root");
    let root = temp.path().join("root");
    let moved = temp.path().join("root-moved");
    let replacement = temp.path().join("replacement");
    ensure_private_dir(&root).unwrap();
    ensure_private_dir(&replacement).unwrap();

    let name = "p424244-held";
    let original_namespace = ensure_canonical_direct_child(&root, name).unwrap();
    let held_lease =
        HeldAdvisoryLock::try_lock(open_instance_lease(&original_namespace, true).unwrap())
            .unwrap();

    let replacement_namespace = ensure_canonical_direct_child(&replacement, name).unwrap();
    drop(open_instance_lease(&replacement_namespace, true).unwrap());

    let budgets = crate::control::ArtifactBudgets::shipping();
    let queue = ArtifactCleanupQueuePermit::try_reserve(&budgets).expect("test cleanup queue");
    let admission = budgets
        .try_reserve_cleanup_for_path(&root)
        .expect("test cleanup admission");
    let mut sweep = DeadNamespaceSweep::begin(&root, "p1-current".into(), queue, admission)
        .expect("pinned sweep");
    let mut swapped = false;
    assert!(sweep.run_batch_with_hook(
        DEAD_INSTANCE_DELETE_LIMIT,
        |_| false,
        || {
            if !swapped {
                std::fs::rename(&root, &moved).unwrap();
                std::fs::rename(&replacement, &root).unwrap();
                swapped = true;
            }
        }
    ));
    assert!(swapped, "the race hook ran immediately before lease open");
    assert!(
        moved.join(name).is_dir(),
        "lease authority must be opened through the retained namespace handle"
    );
    assert!(
        root.join(name).is_dir(),
        "the replacement namespace is outside the sweep authority"
    );

    drop(sweep);
    std::fs::rename(&root, &replacement).unwrap();
    std::fs::rename(&moved, &root).unwrap();
    drop(held_lease);

    let begin = |root: &Path| {
        let budgets = crate::control::ArtifactBudgets::shipping();
        let admission = budgets
            .try_reserve_cleanup_for_path(root)
            .expect("test cleanup admission");
        let queue = ArtifactCleanupQueuePermit::try_reserve(&budgets).expect("test cleanup queue");
        DeadNamespaceSweep::begin(root, "p1-current".into(), queue, admission)
            .expect("pinned sweep")
    };
    let mut sweep = begin(&root);
    let mut swapped = false;
    assert!(sweep.run_batch_with_hook(
        DEAD_INSTANCE_DELETE_LIMIT,
        |_| false,
        || {
            if !swapped {
                std::fs::rename(&root, &moved).unwrap();
                std::fs::rename(&replacement, &root).unwrap();
                swapped = true;
            }
        }
    ));
    assert!(swapped && moved.join(name).is_dir());
    drop(sweep);
    std::fs::rename(&root, &replacement).unwrap();
    std::fs::rename(&moved, &root).unwrap();

    let tombstone = format!("{DEAD_INSTANCE_TOMBSTONE_PREFIX}left");
    std::fs::create_dir(root.join(&tombstone)).unwrap();
    let mut sweep = begin(&root);
    std::fs::rename(&root, &moved).unwrap();
    assert!(sweep.run_batch(|_| false));
    assert!(moved.join(&tombstone).is_dir());
    drop(sweep);

    std::fs::rename(&moved, &root).unwrap();
    race_at(RaceWindow::CursorPinned, {
        let (root, moved, replacement) = (root.clone(), moved.clone(), replacement.clone());
        move || {
            std::fs::rename(&root, &moved).unwrap();
            std::fs::rename(&replacement, &root).unwrap();
        }
    });
    let budgets = crate::control::ArtifactBudgets::shipping();
    let admission = budgets
        .try_reserve_cleanup_for_path(&root)
        .expect("test cleanup admission");
    let queue = ArtifactCleanupQueuePermit::try_reserve(&budgets).expect("test cleanup queue");
    let raced = DeadNamespaceSweep::begin(&root, "p1-current".into(), queue, admission);
    assert!(race_ran());
    assert!(raced.is_err());
    drop(raced);

    let budgeted = temp.path().join("budgeted");
    ensure_private_dir(&budgeted).unwrap();
    let dead = ["p424250-dead", "p424251-dead"];
    for name in dead {
        let namespace = ensure_canonical_direct_child(&budgeted, name).unwrap();
        drop(open_instance_lease(&namespace, true).unwrap());
    }
    let mut sweep = begin(&budgeted);
    assert!(!sweep.run_batch_with_hook(1, |_| false, || {}));
    let left = dead.iter().filter(|name| budgeted.join(name).is_dir());
    assert_eq!(left.count(), 1);
    for entry in std::fs::read_dir(&budgeted).unwrap().flatten() {
        assert!(
            entry.path().join(INSTANCE_LEASE_FILE).is_file(),
            "{entry:?}"
        );
    }
}

/// The sweep lets go of a dead instance's lease by `LOCK_UN` on every way out, here the
/// `continue` after the identity re-check that follows the take: a lease released by the
/// close alone stays locked in a child forked under the sweep until it execs, and the next
/// sweep reads the dead instance as live.
#[cfg(unix)]
#[test]
fn a_dead_instances_lease_is_free_while_a_child_forked_under_the_sweep_has_yet_to_exec() {
    let temp = aterm_tempfile::tempdir().expect("temporary namespace root");
    let root = temp.path().join("root");
    let aside = temp.path().join("aside");
    ensure_private_dir(&root).unwrap();
    ensure_private_dir(&aside).unwrap();
    let name = "p424245-dead";
    let namespace = ensure_canonical_direct_child(&root, name).unwrap();
    drop(open_instance_lease(&namespace, true).unwrap());
    let moved = aside.join(name);

    let budgets = crate::control::ArtifactBudgets::shipping();
    let queue = ArtifactCleanupQueuePermit::try_reserve(&budgets).expect("test cleanup queue");
    let admission = budgets
        .try_reserve_cleanup_for_path(&root)
        .expect("test cleanup admission");
    let mut sweep = DeadNamespaceSweep::begin(&root, "p1-current".into(), queue, admission)
        .expect("pinned sweep");
    let armed = crate::parked_fork::arm_at(1);
    let mut moved_aside = false;
    assert!(sweep.run_batch_with_hook(
        DEAD_INSTANCE_DELETE_LIMIT,
        |_| true,
        || {
            if !moved_aside {
                std::fs::rename(&namespace, &moved).unwrap();
                moved_aside = true;
            }
        }
    ));
    drop(sweep);
    assert!(moved_aside, "the hook ran before the lease was taken");
    let child = armed.child();
    assert!(child.still_parked(), "forked under the sweep's lease");
    assert!(
        moved.is_dir(),
        "the identity re-check failed, so the sweep left the namespace alone"
    );

    let probe = HeldAdvisoryLock::try_lock(open_instance_lease(&moved, false).unwrap());
    assert!(
        probe.is_ok(),
        "the sweep's lease stayed locked in the child forked under it: the sweep \
         let it go by the close alone"
    );
    drop(probe);

    std::fs::rename(&moved, root.join(name)).unwrap();
    sweep_dead_instance_namespaces_with(&root, "p1-current", |_| true);
    assert!(
        !root.join(name).exists(),
        "a dead instance's namespace is swept while the forked child has yet to exec"
    );
    assert!(child.still_parked(), "…while the child had yet to exec");
    child.release();
}

#[test]
fn dead_namespace_debt_after_cursor_open_restarts_with_fresh_generation() {
    let temp = aterm_tempfile::tempdir().expect("temporary namespace root");
    let root = std::fs::canonicalize(temp.path()).unwrap();
    pending_dead_namespace_sweeps()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(root.clone(), PendingDeadNamespaceSweep { dirty: false });

    let budgets = crate::control::ArtifactBudgets::shipping();
    let admission = budgets
        .try_reserve_cleanup_for_path(&root)
        .expect("test cleanup admission");
    let mut sweep = DeadNamespaceSweep::begin(
        &root,
        "p1-current".into(),
        ArtifactCleanupQueuePermit::try_reserve(&budgets).expect("test cleanup queue"),
        admission,
    )
    .expect("initial cursor");

    schedule_dead_instance_namespace_sweep(
        &root,
        "p1-current",
        ArtifactCleanupQueuePermit::try_reserve(&budgets).expect("test cleanup queue"),
    );
    assert!(sweep.run_batch(|_| false));

    let (sender, receiver) = std::sync::mpsc::channel();
    finish_or_restart_dead_namespace_cursor(sweep, &sender);
    let ArtifactCleanupTask::DeadNamespaces {
        root: restarted_root,
        current,
        _queue,
    } = receiver.try_recv().expect("dirty debt schedules a restart")
    else {
        panic!("dirty debt must restart from a new namespace cursor");
    };
    assert_eq!(restarted_root, root);
    assert_eq!(current, "p1-current");
    finish_or_restart_dead_namespace_sweep(restarted_root, current, _queue, &sender);
    assert!(receiver.try_recv().is_err());
    assert!(
        !pending_dead_namespace_sweeps()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains_key(&root)
    );

    let wide = root.join("wide");
    std::fs::create_dir(&wide).unwrap();
    for entry in 0..=DEAD_INSTANCE_SCAN_LIMIT {
        std::fs::write(wide.join(format!("clutter-{entry}")), b"").unwrap();
    }
    let task = ArtifactCleanupTask::DeadNamespaces {
        root: wide,
        current: "p1-current".into(),
        _queue: ArtifactCleanupQueuePermit::try_reserve(&budgets).expect("test cleanup queue"),
    };
    run_best_effort_cleanup_task(task, &sender, &budgets);
    assert!(matches!(
        receiver.try_recv(),
        Ok(ArtifactCleanupTask::DeadNamespaceContinuation(_))
    ));
}

/// Run the shipped worker loop, `artifact_cleanup_worker`, over a private queue seeded
/// with `tasks` and an already-closed wake channel, and return once that loop does.
fn drain_private_cleanup_worker(tasks: Vec<ArtifactCleanupTask>) {
    let (video_sender, video_receiver) = std::sync::mpsc::channel::<VideoRetentionTask>();
    let (best_effort, best_effort_receiver) = std::sync::mpsc::channel();
    let (wake, wake_receiver) = std::sync::mpsc::sync_channel::<()>(1);
    drop(video_sender);
    drop(wake);
    for task in tasks {
        best_effort
            .send(task)
            .expect("the private cleanup queue is open");
    }
    let (returned, worker_returned) = std::sync::mpsc::sync_channel(1);
    let worker = std::thread::Builder::new()
        .name("private-artifact-cleanup-worker".into())
        .spawn(move || {
            artifact_cleanup_worker(
                &video_receiver,
                &best_effort_receiver,
                &wake_receiver,
                &best_effort,
                &crate::control::ArtifactBudgets::shipping(),
            );
            let _ = returned.send(());
        })
        .expect("spawn the private cleanup worker");
    match worker_returned.recv_timeout(std::time::Duration::from_secs(60)) {
        Ok(()) => worker.join().expect("the private cleanup worker returned"),
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            std::panic::resume_unwind(worker.join().expect_err("the worker unwound"));
        }
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            panic!("the private cleanup worker never drained its queue after 60 s")
        }
    }
}

#[test]
fn cleanup_worker_recovers_both_pending_debts_after_task_panics() {
    let temp = aterm_tempfile::tempdir().expect("temporary namespace root");
    let root = std::fs::canonicalize(temp.path()).unwrap();
    let name = "p424245-panic-recovery";
    let namespace = ensure_canonical_direct_child(&root, name).unwrap();
    drop(open_instance_lease(&namespace, true).unwrap());
    pending_dead_namespace_sweeps()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(root.clone(), PendingDeadNamespaceSweep { dirty: false });

    let video_root = ensure_canonical_direct_child(&root, "video-tombstones").unwrap();
    let tombstone = ensure_canonical_direct_child(&video_root, ".prune-panic").unwrap();
    std::fs::write(tombstone.join("partial"), b"partial").unwrap();
    let pinned = crate::pinned_dir::PinnedDir::open(&video_root).unwrap();
    let expected_root = pinned.retained_identity().unwrap();
    drop(pinned);
    pending_video_tombstone_sweeps()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push(PendingVideoTombstoneSweep {
            root: video_root.clone(),
            expected_root,
            dirty: false,
        });

    let budgets = crate::control::ArtifactBudgets::shipping();
    drain_private_cleanup_worker(vec![
        ArtifactCleanupTask::PanicForTest(ArtifactCleanupPanicRecovery::DeadNamespaces {
            root: root.clone(),
            current: "p1-current".into(),
            queue: ArtifactCleanupQueuePermit::try_reserve(&budgets).expect("test cleanup queue"),
        }),
        ArtifactCleanupTask::PanicForTest(ArtifactCleanupPanicRecovery::VideoTombstones {
            root: video_root.clone(),
            expected_root,
            queue: ArtifactCleanupQueuePermit::try_reserve(&budgets).expect("test cleanup queue"),
        }),
    ]);

    assert!(
        !namespace.exists(),
        "panic recovery must requeue the pending root on a fresh cursor"
    );
    assert!(
        !pending_dead_namespace_sweeps()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains_key(&root),
        "the recovered sweep must reconcile its coalescing registry"
    );
    assert!(
        !tombstone.exists(),
        "video panic recovery must requeue the tombstone cursor"
    );
    assert!(
        !pending_video_tombstone_sweeps()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .any(|entry| { entry.root == video_root && entry.expected_root == expected_root }),
        "the recovered video sweep must reconcile its coalescing registry"
    );
}

/// A video producer's deferred cleanup asks for a tombstone sweep only when
/// its abort left a tombstone; a published recording's asks for none.
#[test]
fn only_an_armed_video_cleanup_asks_for_a_tombstone_sweep() {
    let temp = aterm_tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temp.path()).unwrap();
    let identity = crate::pinned_dir::PinnedDir::open(&root)
        .unwrap()
        .retained_identity()
        .unwrap();
    let dead_root = root.join("dead");
    pending_video_tombstone_sweeps()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push(PendingVideoTombstoneSweep {
            root: root.clone(),
            expected_root: identity,
            dirty: false,
        });
    pending_dead_namespace_sweeps()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(
            dead_root.clone(),
            PendingDeadNamespaceSweep { dirty: false },
        );
    let requested = || {
        let video = pending_video_tombstone_sweeps()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .any(|entry| entry.root == root && entry.dirty);
        let dead = pending_dead_namespace_sweeps()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&dead_root)
            .is_some_and(|entry| entry.dirty);
        (video, dead)
    };
    let budgets = crate::control::ArtifactBudgets::shipping();
    let queue = || ArtifactCleanupQueuePermit::try_reserve(&budgets);
    let deferred = |armed: bool| {
        let mut cleanup = DeferredVideoCleanup {
            tombstone_root: root.clone(),
            tombstone_identity: identity,
            tombstone_queue: queue(),
            tombstone_armed: false,
            dead_root: dead_root.clone(),
            current: "p1-current".into(),
            dead_queue: queue(),
        };
        if armed {
            cleanup.arm_tombstone();
        }
        cleanup
    };

    drop(deferred(false));
    assert_eq!(requested(), (false, true));
    drop(deferred(true));
    assert_eq!(requested(), (true, true));
    clear_pending_video_tombstone_sweep(&root, identity);
    clear_pending_dead_namespace_sweep(&dead_root);
}

/// The automatic-image sweep prunes only the directory it was armed for:
/// a replacement standing at the same path is not its to prune.
#[test]
fn the_automatic_image_sweep_prunes_only_the_directory_it_was_armed_for() {
    let temp = aterm_tempfile::tempdir().unwrap();
    let auto = std::fs::canonicalize(temp.path())
        .unwrap()
        .join(AUTO_IMAGES_DIR);
    let dir = auto.join(process_instance_id());
    std::fs::create_dir_all(&dir).unwrap();
    let identity = |dir: &Path| {
        crate::pinned_dir::PinnedDir::open(dir)
            .unwrap()
            .retained_identity()
            .unwrap()
    };
    let armed_for = identity(&dir);
    std::fs::rename(&dir, auto.join("moved")).unwrap();
    std::fs::create_dir(&dir).unwrap();
    let name = |sequence| automatic_capture_name_for("image", process_instance_id(), sequence);
    for sequence in 0..=(AUTO_IMAGE_KEEP as u64 + 2) {
        std::fs::write(dir.join(name(sequence)), b"png").unwrap();
    }
    let fresh = std::ffi::OsString::from(name(AUTO_IMAGE_KEEP as u64 + 2));
    let lookalike = dir.join(name(AUTO_IMAGE_KEEP as u64 + 3));
    std::fs::create_dir(&lookalike).unwrap();
    let in_flight = dir.join(format!(".aterm-write-p1-{:020}", 0));
    std::fs::write(&in_flight, b"partial").unwrap();
    let budgets = crate::control::ArtifactBudgets::shipping();
    let sweep = |expected_dir| ArtifactCleanupTask::AutomaticImages {
        dir: dir.clone(),
        expected_dir,
        fresh: fresh.clone(),
        _queue: ArtifactCleanupQueuePermit::try_reserve(&budgets).expect("test cleanup queue"),
    };
    let count = || {
        std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "png"))
            .count()
    };

    drain_private_cleanup_worker(vec![sweep(armed_for)]);
    assert_eq!(
        count(),
        AUTO_IMAGE_KEEP + 3,
        "the replacement is left whole"
    );
    #[cfg(unix)]
    {
        let raced = auto.join("raced");
        race_at(RaceWindow::ImagesPinned, {
            let (dir, raced) = (dir.clone(), raced.clone());
            move || {
                std::fs::rename(&dir, &raced).unwrap();
                std::fs::create_dir(&dir).unwrap();
            }
        });
        let (requeue, _requeued) = std::sync::mpsc::channel();
        run_best_effort_cleanup_task(sweep(identity(&dir)), &requeue, &budgets);
        assert!(race_ran());
        std::fs::remove_dir(&dir).unwrap();
        std::fs::rename(&raced, &dir).unwrap();
        assert_eq!(count(), AUTO_IMAGE_KEEP + 3);
    }
    drain_private_cleanup_worker(vec![sweep(identity(&dir))]);
    assert_eq!(
        count(),
        AUTO_IMAGE_KEEP,
        "positive control: armed for it, it is pruned"
    );
    assert!(lookalike.is_dir() && in_flight.is_file());
}

/// The tombstone sweep removes `.prune-*` entries only, and only under the
/// root it was armed for.
#[test]
fn the_video_tombstone_sweep_removes_only_tombstones_of_its_own_root() {
    let temp = aterm_tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temp.path()).unwrap().join("instance");
    std::fs::create_dir(&root).unwrap();
    let identity = |dir: &Path| {
        crate::pinned_dir::PinnedDir::open(dir)
            .unwrap()
            .retained_identity()
            .unwrap()
    };
    let armed_for = identity(&root);
    std::fs::rename(&root, temp.path().join("moved")).unwrap();
    for entry in [".prune-x", "rec-kept"] {
        std::fs::create_dir_all(root.join(entry)).unwrap();
        std::fs::write(root.join(entry).join("index.json"), b"{}").unwrap();
    }
    let budgets = crate::control::ArtifactBudgets::shipping();
    let sweep = |expected_root| ArtifactCleanupTask::VideoTombstones {
        root: root.clone(),
        expected_root,
        _queue: ArtifactCleanupQueuePermit::try_reserve(&budgets).expect("test cleanup queue"),
    };

    drain_private_cleanup_worker(vec![sweep(armed_for)]);
    assert!(
        root.join(".prune-x").is_dir(),
        "a replacement root is not its to sweep"
    );
    drain_private_cleanup_worker(vec![sweep(identity(&root))]);
    assert!(
        !root.join(".prune-x").exists(),
        "positive control: its own tombstone goes"
    );
    assert!(
        root.join("rec-kept").is_dir(),
        "a recording is no tombstone"
    );

    let begin = |root: &Path| {
        let budgets = crate::control::ArtifactBudgets::shipping();
        let admission = budgets
            .try_reserve_cleanup_for_path(root)
            .expect("test cleanup admission");
        let queue = ArtifactCleanupQueuePermit::try_reserve(&budgets).expect("test cleanup queue");
        VideoTombstoneSweep::begin(root, identity(root), queue, admission).expect("cursor")
    };
    #[cfg(unix)]
    {
        std::fs::create_dir(root.join(".prune-y")).unwrap();
        let mut cursor = begin(&root);
        let aside = root.with_file_name("aside");
        std::fs::rename(&root, &aside).unwrap();
        assert!(cursor.run_batch());
        assert!(aside.join(".prune-y").is_dir());
        drop(cursor);

        std::fs::rename(&aside, &root).unwrap();
        let expected_root = identity(&root);
        race_at(RaceWindow::CursorPinned, {
            let (root, aside) = (root.clone(), aside.clone());
            move || {
                std::fs::rename(&root, &aside).unwrap();
                std::fs::create_dir(&root).unwrap();
            }
        });
        let budgets = crate::control::ArtifactBudgets::shipping();
        let admission = budgets
            .try_reserve_cleanup_for_path(&root)
            .expect("test cleanup admission");
        let queue = ArtifactCleanupQueuePermit::try_reserve(&budgets).expect("test cleanup queue");
        let raced = VideoTombstoneSweep::begin(&root, expected_root, queue, admission);
        assert!(race_ran());
        assert!(raced.is_none());
    }

    let partial = root.with_file_name("partial");
    std::fs::create_dir_all(partial.join(".prune-a")).unwrap();
    for file in ["one", "two"] {
        std::fs::write(partial.join(".prune-a").join(file), b"").unwrap();
    }
    let mut cursor = begin(&partial);
    assert!(!cursor.run_batch_within(VIDEO_SCAN_WORK + VIDEO_DELETE_WORK));
    assert!(cursor.restart_required);
    let expected_root = cursor.expected_root;
    pending_video_tombstone_sweeps()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push(PendingVideoTombstoneSweep {
            root: partial.clone(),
            expected_root,
            dirty: false,
        });
    let (sender, receiver) = std::sync::mpsc::channel();
    finish_or_restart_video_tombstone_sweep(cursor, &sender);
    clear_pending_video_tombstone_sweep(&partial, expected_root);
    assert!(matches!(
        receiver.try_recv(),
        Ok(ArtifactCleanupTask::VideoTombstones { .. })
    ));
    let short = root.with_file_name("short");
    for tombstone in [".prune-b", ".prune-c"] {
        std::fs::create_dir_all(short.join(tombstone)).unwrap();
    }
    let mut cursor = begin(&short);
    assert!(!cursor.run_batch_within(VIDEO_SCAN_WORK * 3 / 2 + VIDEO_DELETE_WORK));
    assert!(cursor.restart_required);
    drop(cursor);

    let short_identity = identity(&short);
    let pending = || {
        pending_video_tombstone_sweeps()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    };
    pending().push(PendingVideoTombstoneSweep {
        root: short.clone(),
        expected_root: short_identity,
        dirty: false,
    });
    schedule_video_tombstone_sweep(
        &short,
        short_identity,
        ArtifactCleanupQueuePermit::try_reserve(&budgets).expect("test cleanup queue"),
    );
    let dirty: Vec<bool> = pending()
        .iter()
        .filter(|entry| entry.root == short)
        .map(|entry| entry.dirty)
        .collect();
    assert_eq!(dirty, [true]);
    let (sender, receiver) = std::sync::mpsc::channel();
    for restarts in [true, false] {
        let mut cursor = begin(&short);
        assert!(cursor.run_batch());
        finish_or_restart_video_tombstone_sweep(cursor, &sender);
        assert_eq!(receiver.try_recv().is_ok(), restarts);
    }
    assert!(pending().iter().all(|entry| entry.root != short));

    let wide = root.with_file_name("wide");
    std::fs::create_dir(&wide).unwrap();
    for entry in 0..=VIDEO_PRUNE_SCAN_LIMIT {
        std::fs::write(wide.join(format!("clutter-{entry}")), b"").unwrap();
    }
    let task = ArtifactCleanupTask::VideoTombstones {
        expected_root: identity(&wide),
        root: wide,
        _queue: ArtifactCleanupQueuePermit::try_reserve(&budgets).expect("test cleanup queue"),
    };
    run_best_effort_cleanup_task(task, &sender, &budgets);
    assert!(matches!(
        receiver.try_recv(),
        Ok(ArtifactCleanupTask::VideoTombstoneContinuation(_))
    ));
}

#[test]
fn instance_sweep_decision_is_total_and_lease_authoritative() {
    use InstanceLeaseState::{Acquirable, Held, Invalid, Missing};
    use InstanceSweepDecision::{Keep, Remove};

    for pid_alive in [false, true] {
        assert_eq!(
            decide_instance_namespace_sweep(Acquirable, pid_alive),
            Remove,
            "an acquirable exact lease proves abandonment regardless of PID reuse"
        );
        assert_eq!(
            decide_instance_namespace_sweep(Held, pid_alive),
            Keep,
            "a held exact lease always protects its namespace"
        );
        assert_eq!(
            decide_instance_namespace_sweep(Invalid, pid_alive),
            Keep,
            "malformed or unreadable lease state fails closed"
        );
    }
    assert_eq!(decide_instance_namespace_sweep(Missing, true), Keep);
    assert_eq!(decide_instance_namespace_sweep(Missing, false), Remove);
}

#[test]
fn instance_sweep_refuses_redirected_or_malformed_leases() {
    let root = std::env::temp_dir().join(format!(
        "aterm-instance-lease-invalid-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    ensure_private_dir(&root).unwrap();

    let malformed = ensure_canonical_direct_child(&root, "p424244-malformed").unwrap();
    std::fs::create_dir(malformed.join(INSTANCE_LEASE_FILE)).unwrap();
    sweep_dead_instance_namespaces_with(&root, "p1-current", |_| false);
    assert!(
        malformed.is_dir(),
        "a malformed lease fails closed instead of falling back to PID"
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;

        let outside = std::env::temp_dir().join(format!(
            "aterm-instance-lease-outside-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&outside);
        ensure_private_dir(&outside).unwrap();
        let redirected = root.join("p424245-redirected");
        symlink(&outside, &redirected).unwrap();
        sweep_dead_instance_namespaces_with(&root, "p1-current", |_| false);
        assert!(
            std::fs::symlink_metadata(&redirected)
                .unwrap()
                .file_type()
                .is_symlink(),
            "a redirected namespace entry is refused, never followed"
        );
        assert!(
            outside.is_dir(),
            "the redirect target is never recursively removed"
        );

        let bad_lease = ensure_canonical_direct_child(&root, "p424246-linklease").unwrap();
        let victim = root.join("lease-victim");
        std::fs::write(&victim, b"keep").unwrap();
        symlink(&victim, bad_lease.join(INSTANCE_LEASE_FILE)).unwrap();
        sweep_dead_instance_namespaces_with(&root, "p1-current", |_| false);
        assert!(bad_lease.is_dir(), "a symlink lease fails closed");
        assert_eq!(std::fs::read(&victim).unwrap(), b"keep");

        use std::os::unix::fs::PermissionsExt as _;
        let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode();
        let linked = ensure_canonical_direct_child(&root, "p424247-hardlease").unwrap();
        std::fs::hard_link(&victim, linked.join(INSTANCE_LEASE_FILE)).unwrap();
        let victim_mode = mode(&victim);
        assert_eq!(
            open_instance_lease(&linked, true).unwrap_err().kind(),
            std::io::ErrorKind::PermissionDenied
        );
        assert_eq!(mode(&victim), victim_mode);

        let loose = ensure_canonical_direct_child(&root, "p424248-looselease").unwrap();
        let loose_lease = loose.join(INSTANCE_LEASE_FILE);
        std::fs::write(&loose_lease, b"").unwrap();
        std::fs::set_permissions(&loose_lease, std::fs::Permissions::from_mode(0o644)).unwrap();
        drop(open_instance_lease(&loose, false).unwrap());
        assert_eq!(mode(&loose_lease) & 0o7777, 0o600);

        race_at(RaceWindow::LockOpened, {
            let lease = loose_lease.clone();
            move || {
                std::fs::rename(&lease, lease.with_extension("aside")).unwrap();
                std::fs::write(&lease, b"").unwrap();
            }
        });
        let swapped = open_instance_lease(&loose, false);
        assert!(race_ran());
        assert_eq!(
            swapped.unwrap_err().kind(),
            std::io::ErrorKind::PermissionDenied
        );
        race_at(RaceWindow::LockOpened, {
            let namespace = loose.clone();
            move || {
                let behind = namespace.with_extension("behind");
                std::fs::rename(&namespace, &behind).unwrap();
                symlink(&behind, &namespace).unwrap();
            }
        });
        let behind_link = open_instance_lease(&loose, false);
        assert!(race_ran());
        assert_eq!(
            behind_link.unwrap_err().kind(),
            std::io::ErrorKind::PermissionDenied
        );

        let piped = ensure_canonical_direct_child(&root, "p424249-fifolease").unwrap();
        let piped_lease = piped.join(INSTANCE_LEASE_FILE);
        std::fs::write(&piped_lease, b"").unwrap();
        race_at(RaceWindow::LockLooked, {
            let lease = piped_lease.clone();
            move || {
                use std::os::unix::ffi::OsStrExt as _;
                std::fs::remove_file(&lease).unwrap();
                let fifo = std::ffi::CString::new(lease.as_os_str().as_bytes()).unwrap();
                // SAFETY: a valid NUL-terminated path; mkfifo reads it and nothing else.
                assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
                race_at(RaceWindow::LockOpened, move || {
                    std::fs::remove_file(&lease).unwrap();
                });
            }
        });
        let piped_open = open_instance_lease(&piped, false);
        assert!(race_ran());
        assert_eq!(
            piped_open.unwrap_err().kind(),
            std::io::ErrorKind::PermissionDenied
        );

        let refused = ensure_canonical_direct_child(&root, "p424250-refusedlease").unwrap();
        let refused_lease = refused.join(INSTANCE_LEASE_FILE);
        race_at(RaceWindow::LockOpened, {
            let (lease, second) = (refused_lease.clone(), refused.join("second"));
            move || std::fs::hard_link(&lease, &second).unwrap()
        });
        let refused_open = open_instance_lease(&refused, true);
        assert!(race_ran());
        assert_eq!(
            refused_open.unwrap_err().kind(),
            std::io::ErrorKind::PermissionDenied
        );
        assert_eq!(mode(&refused_lease) & 0o7777, 0o600);
        let _ = std::fs::remove_dir_all(outside);
    }

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
#[cfg(unix)]
fn video_prune_refuses_symlink_recordings_and_completion_markers() {
    use std::os::unix::fs::symlink;

    let dir = std::env::temp_dir().join(format!("aterm-video-prune-links-{}", std::process::id()));
    let outside = std::env::temp_dir().join(format!(
        "aterm-video-prune-links-outside-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&outside);
    ensure_private_dir(&dir).unwrap();
    ensure_private_dir(&outside).unwrap();
    std::fs::write(outside.join("index.json"), b"{\"frames\":[]}").unwrap();
    std::fs::write(outside.join(VIDEO_PUBLISHED_FILE), b"outside-marker").unwrap();

    let instance = format!("p{}-prunelinks", std::process::id());
    let bootstrap = confine_video_dir_for_instance(&dir, &instance).unwrap();
    let root = bootstrap.path().parent().unwrap().to_path_buf();
    drop(bootstrap);

    let linked_recording = root.join("rec-00000000000000000000-000");
    symlink(&outside, &linked_recording).unwrap();
    let linked_index_recording = root.join("rec-00000000000000000001-000");
    std::fs::create_dir(&linked_index_recording).unwrap();
    symlink(
        outside.join("index.json"),
        linked_index_recording.join("index.json"),
    )
    .unwrap();
    std::fs::write(
        linked_index_recording.join(VIDEO_PUBLISHED_FILE),
        b"published",
    )
    .unwrap();
    let linked_marker_recording = root.join("rec-00000000000000000002-000");
    std::fs::create_dir(&linked_marker_recording).unwrap();
    std::fs::write(
        linked_marker_recording.join("index.json"),
        b"{\"frames\":[]}",
    )
    .unwrap();
    symlink(
        outside.join(VIDEO_PUBLISHED_FILE),
        linked_marker_recording.join(VIDEO_PUBLISHED_FILE),
    )
    .unwrap();
    for i in 10..18 {
        let recording = root.join(format!("rec-{i:020}-000"));
        std::fs::create_dir(&recording).unwrap();
        std::fs::write(recording.join("index.json"), b"{\"frames\":[]}").unwrap();
        std::fs::write(recording.join(VIDEO_PUBLISHED_FILE), b"published").unwrap();
    }

    let mut fresh = confine_video_dir_for_instance(&dir, &instance).unwrap();
    let fresh_path = fresh.path().to_path_buf();
    let index = fresh
        .write_new_private(std::ffi::OsStr::new("index.json"), b"{\"frames\":[]}")
        .unwrap();
    let _ = fresh.publish(&[], &index).unwrap();
    drop(fresh.publish_marker().unwrap());
    drop(index);
    drop(fresh);
    wait_for_artifact_cleanup(&fresh_path);

    assert!(
        !root.join("rec-00000000000000000010-000").exists(),
        "positive control: retention prunes the oldest genuine predecessor"
    );
    assert!(
        std::fs::symlink_metadata(&linked_recording)
            .unwrap()
            .file_type()
            .is_symlink(),
        "a symlink recording is never considered a retention candidate"
    );
    assert!(
        std::fs::symlink_metadata(linked_index_recording.join("index.json"))
            .unwrap()
            .file_type()
            .is_symlink(),
        "a symlink completion index cannot make a recording pruneable"
    );
    assert!(
        std::fs::symlink_metadata(linked_marker_recording.join(VIDEO_PUBLISHED_FILE))
            .unwrap()
            .file_type()
            .is_symlink(),
        "a symlink publication marker cannot make a recording pruneable"
    );
    assert_eq!(
        std::fs::read(outside.join("index.json")).unwrap(),
        b"{\"frames\":[]}",
        "retention never follows either redirect"
    );
    assert_eq!(
        std::fs::read(outside.join(VIDEO_PUBLISHED_FILE)).unwrap(),
        b"outside-marker",
        "retention never follows a publication-marker redirect"
    );
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&outside);
}
