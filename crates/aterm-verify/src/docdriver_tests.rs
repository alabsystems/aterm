// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn argv(a: &[&str]) -> Vec<OsString> {
    a.iter().map(OsString::from).collect()
}

/// A doctest pass as cargo spells it: the package's crate as `--test`'s
/// input, beside the `--test-*` options that are not the flag.
fn pass() -> Vec<OsString> {
    argv(&[
        "--edition=2024",
        "--crate-name",
        "plain",
        "--test",
        "crates/plain/src/lib.rs",
        "--test-run-directory",
        "/r/crates/plain",
        "--test-args",
        "--quiet",
    ])
}

#[test]
fn only_a_doctest_pass_of_a_named_package_is_skipped() {
    let skip = OsString::from(skip_list(&["other".into(), "plain".into()]));
    let s = Some(skip.as_os_str());
    assert!(skips(&pass(), Some(OsStr::new("plain")), s));
    assert!(!skips(&pass(), Some(OsStr::new("documented")), s));
    for name in ["plai", "plainer", "lain", ""] {
        assert!(!skips(&pass(), Some(OsStr::new(name)), s), "{name:?}");
    }
    let gappy = OsString::from(",,plain,");
    assert!(skips(&pass(), Some(OsStr::new("plain")), Some(&gappy)));
    assert!(!skips(&pass(), Some(OsStr::new("")), Some(&gappy)));
}

#[test]
fn whatever_the_stand_in_cannot_read_runs() {
    let skip = OsString::from("plain");
    let s = Some(skip.as_os_str());
    let plain = Some(OsStr::new("plain"));
    assert!(!skips(&argv(&["-vV"]), plain, s));
    assert!(!skips(&argv(&["@/tmp/cargo-args"]), plain, s));
    assert!(!skips(
        &argv(&["--test-args", "--quiet", "--test-run-directory", "/r"]),
        plain,
        s
    ));
    assert!(!skips(&pass(), None, s));
    assert!(!skips(&pass(), plain, None));
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;
        let raw = OsStr::from_bytes(b"plain,\xff");
        assert!(!skips(&pass(), plain, Some(raw)));
    }
}

#[derive(Debug)]
struct ProgressHost {
    busy_descendant: bool,
    reads: std::sync::atomic::AtomicU64,
}

impl ProgressHost {
    const fn blocked() -> Self {
        Self {
            busy_descendant: false,
            reads: std::sync::atomic::AtomicU64::new(0),
        }
    }

    const fn busy_descendant() -> Self {
        Self {
            busy_descendant: true,
            reads: std::sync::atomic::AtomicU64::new(0),
        }
    }
}

impl crate::exec::Host for ProgressHost {
    fn group_usage(&self, pgid: u32) -> Option<Vec<aterm_sysprobe::MemberUsage>> {
        Some(vec![aterm_sysprobe::MemberUsage {
            pid: pgid,
            start: 1,
            cpu_ns: self.reads.load(std::sync::atomic::Ordering::SeqCst),
            ..aterm_sysprobe::MemberUsage::default()
        }])
    }

    fn tree_usage(&self, root: u32) -> Option<Vec<crate::exec::TreeMember>> {
        let read = self.reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        let cpu = if self.busy_descendant {
            read
        } else {
            // Let the generated process start and publish its JSON case before the injected
            // clock becomes still. This is progress evidence, not a wall-time allowance.
            read.min(32)
        };
        Some(vec![
            crate::exec::TreeMember {
                pid: root,
                start: 1,
                cpu_ns: 0,
            },
            crate::exec::TreeMember {
                pid: root.saturating_add(1),
                start: 2,
                cpu_ns: cpu,
            },
        ])
    }

    fn usage_every(&self) -> Duration {
        Duration::ZERO
    }

    fn sample(
        &self,
        pid: u32,
        output_dir: &Path,
        _: &dyn Fn() -> bool,
    ) -> crate::exec::StackSample {
        let report = output_dir.join(format!("hung-{pid}.sample.txt"));
        match std::fs::write(&report, b"injected all-thread sample\n") {
            Ok(()) => crate::exec::StackSample {
                report: Some(report),
                error: None,
            },
            Err(error) => crate::exec::StackSample {
                report: None,
                error: Some(error.to_string()),
            },
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
fn fixture_settings(root: &Path, hang_after: Duration) -> Runtool {
    let halt = halt_marker(&root.join("scratch"));
    prepare_halt_marker(&halt).expect("first-red admission");
    Runtool {
        scratch: root.join("scratch"),
        samples: Some(root.join("hung-samples")),
        spec: "-p documented --doc".to_owned(),
        halt: Some(halt),
        hang_after,
    }
}

/// `create_new` transfers no ownership on collision. NEGATIVE CONTROL: deleting the pending
/// path in `prepare_halt_marker`'s error cleanup destroys this pre-existing byte.
#[test]
fn a_pending_collision_survives_failed_protocol_preparation() {
    let root = aterm_tempfile::tempdir().expect("fixture root");
    let marker = halt_marker(root.path());
    let pending = halt_pending(&marker);
    create_halt_file(&pending, b'X').expect("pre-existing pending collision");

    let error = prepare_halt_marker(&marker).expect_err("pending create_new must collide");
    assert!(error.contains("cannot create"), "{error}");
    assert_eq!(
        std::fs::read(&pending).expect("the pre-existing pending survives"),
        b"X"
    );
    assert!(
        !marker.exists(),
        "only this invocation's admission is removed"
    );
}

/// A publication error must not restore OPEN. NEGATIVE CONTROL: leaving the valid pending
/// sentinel intact makes both the live-peer probe and the next admission accept it.
#[test]
fn a_failed_red_rename_poison_closes_live_and_later_admission() {
    let root = aterm_tempfile::tempdir().expect("fixture root");
    let marker = halt_marker(root.path());
    prepare_halt_marker(&marker).expect("first-red protocol");

    let error = signal_halt_marker_with(&marker, |_, _| {
        Err(std::io::Error::other("injected rename refusal"))
    })
    .expect_err("the injected rename fails honestly");
    assert!(
        error.contains("injected rename refusal") && error.contains("poisoned"),
        "{error}"
    );
    let live = external_halt(&marker).expect("a live peer fails closed");
    assert!(live.contains("this peer was stopped"), "{live}");
    let later = admit_halt_marker(&marker).expect_err("a later start fails closed");
    assert!(later.contains("one-byte regular file"), "{later}");
}

/// Publishing RED is a directory operation, not a write through the pending sentinel.
/// NEGATIVE CONTROL: requiring a writable pending handle makes this valid owner-private,
/// read-only sentinel leave the protocol OPEN instead of atomically renaming it.
#[cfg(unix)]
#[test]
fn a_read_only_pending_sentinel_still_publishes_red_by_rename() {
    use std::os::unix::fs::PermissionsExt;

    let root = aterm_tempfile::tempdir().expect("fixture root");
    let marker = halt_marker(root.path());
    prepare_halt_marker(&marker).expect("first-red protocol");
    let pending = halt_pending(&marker);
    std::fs::set_permissions(&pending, std::fs::Permissions::from_mode(0o400))
        .expect("make the validated sentinel read-only");

    signal_halt_marker(&marker).expect("rename publishes RED without writing the sentinel");
    assert!(matches!(
        halt_state(&marker).expect("published state"),
        HaltState::Red
    ));
    assert!(!pending.exists(), "the pending name moved atomically");
    let later = admit_halt_marker(&marker).expect_err("RED refuses every later start");
    assert!(later.contains("stopped before starting"), "{later}");
}

/// A publisher that loses the pending-to-RED rename race accepts the winner's valid RED.
/// NEGATIVE CONTROL: returning the missing-pending error before re-reading RED makes this
/// deterministic interleaving report a false publication failure.
#[test]
fn a_concurrent_red_publish_accepts_the_winner_before_pending_open() {
    let root = aterm_tempfile::tempdir().expect("fixture root");
    let marker = halt_marker(root.path());
    prepare_halt_marker(&marker).expect("first-red protocol");

    signal_halt_marker_after(
        &marker,
        || signal_halt_marker(&marker).expect("the competing publisher wins"),
        |from, to| std::fs::rename(from, to),
    )
    .expect("the losing publisher accepts the valid RED");
    assert!(matches!(
        halt_state(&marker).expect("published state"),
        HaltState::Red
    ));
}

/// RED never waits behind the admission lock. A start that has proved actual contention
/// acquires the lock only after RED is visible and then refuses before exec. NEGATIVE
/// CONTROL: serializing publication behind this holder misses the one-minute hang detector;
/// checking the protocol before taking its lock lets the sentinel appear.
#[cfg(unix)]
#[test]
fn first_red_overtakes_a_start_contending_on_admission() {
    use aterm_spec::wait::MUST_HAPPEN;

    let root = aterm_tempfile::tempdir().expect("fixture root");
    std::fs::create_dir(root.path().join("scratch")).expect("scratch");
    let settings = fixture_settings(root.path(), Duration::ZERO);
    let marker = settings.halt.clone().expect("armed runtool");
    let holder = open_halt_file(&marker, HALT_ADMISSION)
        .expect("valid admission")
        .expect("admission exists");
    aterm_uds::file_lock::lock_exclusive_until(
        &holder,
        std::time::Instant::now() + MUST_HAPPEN,
        "fixture could not hold admission",
    )
    .expect("hold admission");

    let sentinel = root.path().join("contended-ran");
    let binary = root.path().join("contended-doctest");
    crate::standin_body(
        &binary,
        &format!(
            "printf ran > '{}'; printf '%s\\n' \\
                 '{{\"type\":\"suite\",\"event\":\"started\",\"test_count\":0}}' \\
                 '{{\"type\":\"suite\",\"event\":\"ok\",\"passed\":0,\"failed\":0,\"ignored\":0,\"measured\":0,\"filtered_out\":0,\"exec_time\":0.0}}'",
            sentinel.display()
        ),
    );

    let (contended_tx, contended_rx) = std::sync::mpsc::channel();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let queued_settings = settings.clone();
    let queued = std::thread::spawn(move || {
        let result = runtool_on_with_admission(
            &[binary.into_os_string()],
            &queued_settings,
            &ProgressHost::busy_descendant(),
            |path| {
                admit_halt_marker_with(path, || {
                    let _ = contended_tx.send(());
                })
            },
        );
        let _ = done_tx.send(result);
    });
    contended_rx
        .recv_timeout(MUST_HAPPEN)
        .expect("the queued runtool actually contended on admission");

    let (signal_tx, signal_rx) = std::sync::mpsc::channel();
    let signal_marker = marker.clone();
    let signaler = std::thread::spawn(move || {
        let _ = signal_tx.send(signal_halt_marker(&signal_marker));
    });
    let signaled = signal_rx.recv_timeout(MUST_HAPPEN);
    if signaled.is_err() {
        let _ = holder.unlock();
        let _ = signaler.join();
        let _ = done_rx.recv_timeout(MUST_HAPPEN);
        let _ = queued.join();
        panic!("RED waited behind the admission holder");
    }
    signaled
        .expect("checked above")
        .expect("atomic RED publication");
    signaler.join().expect("signaler");
    holder.unlock().expect("release admission holder");

    let stopped = done_rx
        .recv_timeout(MUST_HAPPEN)
        .expect("the contended runtool ended")
        .expect_err("RED refuses the contended start");
    queued.join().expect("queued runtool");
    assert!(stopped.contains("stopped before starting"), "{stopped}");
    assert!(!sentinel.exists(), "the contended doctest must not exec");
}

/// The OPEN read is the start-admission linearization point. A holder paused after that read
/// may spawn after RED is published, but it must observe RED immediately and reap the child
/// as NOT RUN. NEGATIVE CONTROL: removing the live probe leaves this fixture blocked forever.
#[cfg(unix)]
#[test]
fn a_start_admitted_before_red_is_stopped_after_its_spawn() {
    use aterm_spec::wait::MUST_HAPPEN;

    let root = aterm_tempfile::tempdir().expect("fixture root");
    std::fs::create_dir(root.path().join("scratch")).expect("scratch");
    let settings = fixture_settings(root.path(), Duration::ZERO);
    let marker = settings.halt.clone().expect("armed runtool");
    let binary = root.path().join("admitted-doctest");
    crate::standin_body(
        &binary,
        "/usr/bin/mkfifo \"$0.pipe\"\nexec /bin/cat \"$0.pipe\"",
    );

    let (admitted_tx, admitted_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let peer_settings = settings.clone();
    let peer = std::thread::spawn(move || {
        let result = runtool_on_with_admission(
            &[binary.into_os_string()],
            &peer_settings,
            &ProgressHost::busy_descendant(),
            |path| {
                let admission = admit_halt_marker(path)?;
                admitted_tx
                    .send(())
                    .map_err(|error| format!("cannot publish fixture admission: {error}"))?;
                release_rx.recv_timeout(MUST_HAPPEN).map_err(|error| {
                    format!("timed out holding fixture admission before spawn: {error}")
                })?;
                Ok(admission)
            },
        );
        let _ = done_tx.send(result);
    });
    admitted_rx
        .recv_timeout(MUST_HAPPEN)
        .expect("the peer passed its OPEN check");
    signal_halt_marker(&marker).expect("publish RED while the admitted peer is paused");
    release_tx.send(()).expect("release admitted peer to spawn");

    let stopped = done_rx
        .recv_timeout(MUST_HAPPEN)
        .expect("the admitted peer observed RED")
        .expect("the admitted peer returns its NOT RUN record");
    peer.join().expect("admitted peer");
    assert!(!stopped.ok && stopped.hung.is_none(), "{stopped:?}");
    assert!(
        stopped
            .spawn_error
            .as_deref()
            .is_some_and(|why| why.contains("another doctest binary reached the first red")),
        "{stopped:?}"
    );
    assert!(stopped.output.contains("NOT RUN"), "{}", stopped.output);
}

/// An admitted peer that remains healthy and consumes descendant CPU still obeys the shared
/// first red. NEGATIVE CONTROL: without the live sentinel probe this busy fixture blocks on
/// its pipe forever after its sibling becomes HUNG.
#[cfg(unix)]
#[test]
fn a_live_busy_doctest_peer_stops_when_another_is_hung() {
    use aterm_spec::wait::{MUST_HAPPEN, until_within};

    let root = aterm_tempfile::tempdir().expect("fixture root");
    std::fs::create_dir(root.path().join("scratch")).expect("scratch");
    let settings = fixture_settings(root.path(), Duration::ZERO);
    let ready = root.path().join("busy-ready");
    let busy = root.path().join("busy-doctest");
    crate::standin_body(
        &busy,
        &format!(
            "/usr/bin/mkfifo \"$0.pipe\"\n\
             printf '%s\\n' \\
                 '{{\"type\":\"suite\",\"event\":\"started\",\"test_count\":1}}' \\
                 '{{\"type\":\"test\",\"event\":\"started\",\"name\":\"docs::busy_peer\"}}'\n\
             printf ready > '{}'\n\
             exec /bin/cat \"$0.pipe\"",
            ready.display()
        ),
    );
    let (peer_tx, peer_rx) = std::sync::mpsc::channel();
    let peer_settings = settings.clone();
    let peer = std::thread::spawn(move || {
        let result = runtool_on(
            &[busy.into_os_string()],
            &peer_settings,
            &ProgressHost::busy_descendant(),
        );
        let _ = peer_tx.send(result);
    });
    until_within(MUST_HAPPEN, "the busy doctest's exec proof", || {
        ready.exists().then_some(())
    });

    let blocked = root.path().join("blocked-doctest");
    crate::standin_body(
        &blocked,
        "printf '%s\\n' \\
             '{\"type\":\"suite\",\"event\":\"started\",\"test_count\":1}' \\
             '{\"type\":\"test\",\"event\":\"started\",\"name\":\"docs::hung_sibling\"}'\n\
         /usr/bin/mkfifo \"$0.pipe\"\n\
         exec /bin/cat \"$0.pipe\"",
    );
    let hung_run = runtool_on(
        &[blocked.into_os_string()],
        &settings,
        &ProgressHost::blocked(),
    )
    .expect("the sibling runtool invocation");
    assert!(hung_run.hung.is_some(), "{hung_run:?}");

    let stopped = peer_rx
        .recv_timeout(MUST_HAPPEN)
        .expect("the live peer observed RED")
        .expect("the admitted peer returns its NOT RUN record");
    peer.join().expect("live peer");
    assert!(!stopped.ok && stopped.hung.is_none(), "{stopped:?}");
    assert!(
        stopped
            .spawn_error
            .as_deref()
            .is_some_and(|why| why.contains("another doctest binary reached the first red")),
        "{stopped:?}"
    );
    assert!(stopped.output.contains("NOT RUN"), "{}", stopped.output);
}

/// A malformed admission fails closed before exec, while a halt-off run has no admission and
/// runs independently of a red file. These are the negative controls for I/O failure and for
/// applying first-red behavior to an ordinary full-sweep verify run.
#[cfg(unix)]
#[test]
fn doctest_first_red_fails_closed_and_halt_off_does_not_adopt_it() {
    let root = aterm_tempfile::tempdir().expect("fixture root");
    let scratch = root.path().join("scratch");
    std::fs::create_dir(&scratch).expect("scratch");
    let marker = halt_marker(&scratch);
    prepare_halt_marker(&marker).expect("first-red admission");
    std::fs::OpenOptions::new()
        .write(true)
        .open(&marker)
        .expect("open admission")
        .set_len(0)
        .expect("mutate admission");
    let sentinel = root.path().join("ran");
    let binary = root.path().join("generated-doctest");
    crate::standin_body(
        &binary,
        &format!(
            "printf ran > '{}'; printf '%s\\n' \\
                 '{{\"type\":\"suite\",\"event\":\"started\",\"test_count\":0}}' \\
                 '{{\"type\":\"suite\",\"event\":\"ok\",\"passed\":0,\"failed\":0,\"ignored\":0,\"measured\":0,\"filtered_out\":0,\"exec_time\":0.0}}'",
            sentinel.display()
        ),
    );
    let armed = Runtool {
        scratch: scratch.clone(),
        samples: None,
        spec: "-p documented --doc".to_owned(),
        halt: Some(marker),
        hang_after: Duration::ZERO,
    };
    let refused = runtool_on(
        &[binary.clone().into_os_string()],
        &armed,
        &ProgressHost::busy_descendant(),
    )
    .expect_err("malformed admission fails closed");
    assert!(refused.contains("one-byte regular file"), "{refused}");
    assert!(!sentinel.exists(), "malformed admission must prevent exec");

    let halt_off = Runtool {
        halt: None,
        ..armed
    };
    let run = runtool_on(
        &[binary.into_os_string()],
        &halt_off,
        &ProgressHost::busy_descendant(),
    )
    .expect("halt-off runtool");
    assert!(run.ok, "{run:?}");
    assert!(sentinel.exists(), "halt-off doctest runs");
}

/// A non-libtest crash is a decisive binary red even though it emits no failed JSON case.
/// NEGATIVE CONTROL: without `runtool_on`'s post-run transition the admission stays OPEN and
/// the final admission succeeds.
#[cfg(unix)]
#[test]
fn a_non_json_doctest_red_closes_first_red_admission() {
    let root = aterm_tempfile::tempdir().expect("fixture root");
    std::fs::create_dir(root.path().join("scratch")).expect("scratch");
    let binary = root.path().join("generated-doctest");
    crate::standin_body(&binary, "exit 9");
    let settings = fixture_settings(root.path(), Duration::ZERO);
    let run = runtool_on(
        &[binary.into_os_string()],
        &settings,
        &ProgressHost::busy_descendant(),
    )
    .expect("the runtool invocation");
    assert!(!run.ok && run.code == Some(9), "{run:?}");
    assert!(run.environment_failure().is_none(), "{run:?}");
    let stopped = admit_halt_marker(settings.halt.as_deref().expect("armed"))
        .expect_err("the crash closes every later start");
    assert!(stopped.contains("stopped before starting"), "{stopped}");
}

/// A generated binary waiting while its descendant consumes CPU is making progress. The
/// fixture itself sleeps, while the injected complete tree advances only the descendant's
/// CPU generation; changing that generation to zero turns this into the hung control above.
#[cfg(unix)]
#[test]
fn a_busy_doctest_descendant_prevents_a_hung_red() {
    let root = aterm_tempfile::tempdir().expect("fixture root");
    std::fs::create_dir(root.path().join("scratch")).expect("scratch");
    let binary = root.path().join("generated-doctest");
    crate::standin_body(
        &binary,
        "printf '%s\\n' \\
             '{\"type\":\"suite\",\"event\":\"started\",\"test_count\":1}' \\
             '{\"type\":\"test\",\"event\":\"started\",\"name\":\"docs::busy_child\"}'\n\
         /bin/sleep 0.05\n\
         printf '%s\\n' \\
             '{\"type\":\"test\",\"event\":\"ok\",\"name\":\"docs::busy_child\",\"exec_time\":0.05}' \\
             '{\"type\":\"suite\",\"event\":\"ok\",\"passed\":1,\"failed\":0,\"ignored\":0,\"measured\":0,\"filtered_out\":0,\"exec_time\":0.05}'",
    );
    let settings = fixture_settings(root.path(), Duration::ZERO);
    let run = runtool_on(
        &[binary.into_os_string()],
        &settings,
        &ProgressHost::busy_descendant(),
    )
    .expect("the runtool invocation");
    assert!(run.ok && run.hung.is_none(), "{run:?}");
    let rendered = crate::libtest_json::render(&run.output, run.stopped_at.as_ref());
    assert!(
        rendered.contains("test docs::busy_child ... ok") && rendered.contains("test result: ok."),
        "{rendered}"
    );
}
