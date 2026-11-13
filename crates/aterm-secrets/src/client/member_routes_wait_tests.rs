// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn refused_or_ended_listeners_keep_one_retry_and_do_not_restart_after_ending() {
    let bell = crate::wake::Bell::default();
    let ending = Stop::new();
    let project = |event| {
        assert!(matches!(event, ProjectionEvent::Ended));
        bell.ring(crate::wake::Wake::Listener);
    };
    std::thread::scope(|scope| {
        let mut owner = Owner::default();
        assert!(
            owner
                .maintain_with(0, false, |recovering| {
                    assert!(!recovering);
                    Err(std::io::Error::new(
                        std::io::ErrorKind::WouldBlock,
                        "injected spawn refusal",
                    ))
                })
                .is_err()
        );
        let first = crate::converge::FIRST_RETRY_MS;
        assert!(
            !owner
                .maintain_with(first - 1, false, |_| panic!("retry not due"))
                .unwrap()
        );
        assert!(
            owner
                .maintain_with(first, false, |recovering| {
                    assert!(recovering);
                    std::thread::Builder::new()
                        .spawn_scoped(scope, || panic!("injected outer worker failure"))
                })
                .unwrap()
        );
        let deadline = Instant::now() + Duration::from_secs(60);
        while !owner.worker.as_ref().unwrap().is_finished() {
            assert!(Instant::now() < deadline, "listener did not finish");
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(
            owner
                .maintain_with(first, false, |_| panic!("failure must be reaped first"))
                .is_err()
        );
        assert!(owner.worker.is_none(), "the failed handle was joined");
        assert!(
            !owner
                .maintain_with(first * 3 - 1, false, |_| panic!("retry not due"))
                .unwrap()
        );
        let (finish, finishing) = std::sync::mpsc::channel();
        let (noticed, noticing) = std::sync::mpsc::channel();
        let (pausing, paused) = std::sync::mpsc::channel();
        bell.ring(crate::wake::Wake::Route);
        let pause_bell = &bell;
        let pause = scope.spawn(move || {
            pausing.send(()).unwrap();
            super::super::pause_on_bell(pause_bell, Duration::from_secs(60))
        });
        paused.recv_timeout(Duration::from_secs(60)).unwrap();
        let notice_flag = Arc::clone(&owner.finishing);
        let ending = &ending;
        let project = &project;
        assert!(
            owner
                .maintain_with(first * 3, false, |recovering| {
                    assert!(recovering);
                    std::thread::Builder::new().spawn_scoped(scope, move || {
                        drop(Notice {
                            finishing: &notice_flag,
                            ending,
                            project,
                        });
                        noticed.send(()).unwrap();
                        finishing.recv_timeout(Duration::from_secs(60)).unwrap();
                    })
                })
                .unwrap()
        );
        noticing.recv_timeout(Duration::from_secs(60)).unwrap();
        assert_eq!(pause.join().unwrap(), crate::wake::Wake::Listener);
        assert!(
            !owner
                .maintain_with(first * 3, false, |_| panic!("one listener at a time"))
                .unwrap(),
            "the exit notice precedes the finished handle"
        );
        assert!(
            !owner
                .maintain_with(first * 3, true, |_| panic!("ending starts no listener"))
                .unwrap()
        );
        finish.send(()).unwrap();
        assert!(owner.finish(ending));
        assert!(
            !owner
                .maintain_with(first * 10, true, |_| panic!("no restart after ending"))
                .unwrap()
        );
    });
    struct PanicsWhileResetting;
    impl Changes for PanicsWhileResetting {
        fn wait(&mut self, _: Option<Instant>, _: &[BorrowedFd<'_>]) -> bool {
            panic!("injected readiness failure before shutdown");
        }
        fn reset(&mut self) {
            panic!("unreaped outer failure at shutdown");
        }
    }
    let root = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(root.path());
    let members = Mutex::new(MembersAsked::default());
    let (ended, ending) = std::sync::mpsc::channel();
    let returned = while_hearing(
        &paths,
        &members,
        &|_| panic!("failed listener admits no member work"),
        &|_| {},
        &|event: ProjectionEvent| {
            if matches!(event, ProjectionEvent::Ended) {
                ended.send(()).unwrap();
            }
        },
        Rings::default(),
        PanicsWhileResetting,
        |_| {
            ending.recv_timeout(Duration::from_secs(60)).unwrap();
            // Return without another maintenance call or a manual join. The
            // actual owner must consume the panic before scope autojoin.
            17
        },
    );
    assert_eq!(returned, 17);

    struct Inert(Arc<std::sync::atomic::AtomicUsize>);
    impl MemberArrivals for Inert {
        fn refresh(&mut self) -> std::io::Result<()> {
            Ok(())
        }
        fn fds(&self) -> Vec<BorrowedFd<'_>> {
            Vec::new()
        }
        fn poll(&mut self) -> std::io::Result<bool> {
            Ok(false)
        }
    }
    impl Drop for Inert {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
    let opened = std::sync::atomic::AtomicUsize::new(0);
    let dropped = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let open = |_: &Paths| -> std::io::Result<Box<dyn MemberArrivals>> {
        if opened.fetch_add(1, Ordering::SeqCst) == 0 {
            return Err(std::io::Error::from_raw_os_error(libc::EMFILE));
        }
        Ok(Box::new(Inert(Arc::clone(&dropped))))
    };
    let first = crate::converge::FIRST_RETRY_MS;
    let mut native = NativeArrivals::default();
    assert!(!native.maintain(&paths, &open, false, 0, &|_| {}));
    assert!(!native.maintain(&paths, &open, false, first - 1, &|_| {}));
    assert_eq!(opened.load(Ordering::SeqCst), 1);
    assert!(native.maintain(&paths, &open, false, first, &|_| {}));
    assert!(!native.maintain(&paths, &open, false, first * 100, &|_| {}));
    assert_eq!(
        opened.load(Ordering::SeqCst),
        2,
        "healthy custody is retained"
    );
    assert!(!native.poll(first * 100, &|_| {}));

    let config = paths.home.join(".config/aterm/aterm.toml");
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    crate::fsx::write_owned_in_place(&config, "[swarm]\nenabled = false\n");
    let forbidden = super::super::MemberCallbacks {
        ask: |_| panic!("readiness never authenticates a member"),
        arrivals: |_| panic!("held or healthy custody opens no new source"),
    };
    let mut listener = Listener::new(&paths, |_: Option<Instant>| false);
    listener.arrivals = native;
    assert!(!listener.maintain_arrivals(&paths, Some(&forbidden), true, false, &|_| {}));
    assert!(listener.arrivals.source.is_none());
    assert_eq!(dropped.load(Ordering::SeqCst), 1, "Off releases the owner");
    crate::fsx::write_owned_in_place(&config, "[swarm]\nenabled = true\n");
    let permitted = super::super::MemberCallbacks {
        ask: forbidden.ask,
        arrivals: |_| {
            Ok(Box::new(Inert(Arc::new(
                std::sync::atomic::AtomicUsize::new(0),
            ))))
        },
    };
    assert!(listener.maintain_arrivals(&paths, Some(&permitted), true, false, &|_| {}));
    assert!(listener.arrivals.source.is_some());
    assert!(listener.due.is_some(), "On owes a fresh ordinary ask");
    assert!(listener.maintain_arrivals(&paths, Some(&forbidden), true, false, &|_| {}));
    assert!(!listener.maintain_arrivals(&paths, Some(&forbidden), false, false, &|_| {}));
    assert!(
        listener.arrivals.source.is_none(),
        "asking ended before a new read"
    );
    assert!(listener.maintain_arrivals(&paths, Some(&permitted), true, false, &|_| {}));
    crate::fsx::write_owned_in_place(&config, "[swarm]\nenabled = 'unknown'\n");
    assert!(!listener.maintain_arrivals(&paths, Some(&forbidden), true, false, &|_| {}));
    assert!(
        listener.arrivals.source.is_none(),
        "unreadable policy holds too"
    );

    aterm_uds::lan::seam_here(
        aterm_uds::lan::Seam {
            socket: Some(paths.home.join("absent-broker.sock")),
            ..aterm_uds::lan::Seam::default()
        },
        || {
            let edge = Instant::now();
            let started = Instant::now();
            let members = Mutex::new(MembersAsked {
                at: Some(started),
                asks: 1,
                ..MembersAsked::default()
            });
            let mut listener = Listener::new(&paths, |_: Option<Instant>| false);
            listener.due = Some(edge);
            listener.edge = Some(edge);
            listener.look = Some(started + Duration::from_secs(60));
            // The main round won the shared ask after this observed edge.
            // Servicing it must not reset the completed ask or read private
            // waiting state again, even though the callback itself is current.
            listener
                .round(
                    &paths,
                    &members,
                    &|_| panic!("this edge is already covered by the fresh ask"),
                    &|_| {},
                    &|_: ProjectionEvent| panic!("no new observation is due"),
                    Rings::default(),
                    &Stop::new(),
                )
                .unwrap();
            let asked = members.lock().unwrap();
            assert_eq!(asked.at, Some(started));
            assert_eq!(asked.asks, 1);
            assert!(listener.due.is_none());
            assert!(listener.edge.is_none());
        },
    );
}
