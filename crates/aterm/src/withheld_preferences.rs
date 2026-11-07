// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Retain deferred-preference intent on the window's existing input keeper.

use aterm_secrets::converge::{Last, RunnerRetry};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread::JoinHandle;

struct Requests(AtomicU64);

impl Requests {
    const fn new() -> Self {
        Self(AtomicU64::new(0))
    }

    fn request(&self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }

    fn current(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

static REQUESTS: Requests = Requests::new();

pub(super) fn request() {
    REQUESTS.request();
}

#[derive(Default)]
pub(super) struct Coordinator {
    seen: u64,
    wanted: Option<u64>,
    worker: Option<(u64, JoinHandle<Last>)>,
    retry: RunnerRetry,
}

impl Coordinator {
    pub(super) fn round(&mut self, now: u64) -> Result<(), String> {
        self.round_with(REQUESTS.current(), now, || {
            std::thread::Builder::new()
                .name("aterm-setup-follow-up".into())
                .spawn(super::finish_withheld_preferences)
        })
    }

    fn round_with(
        &mut self,
        requests: u64,
        now: u64,
        start: impl FnOnce() -> std::io::Result<JoinHandle<Last>>,
    ) -> Result<(), String> {
        if self.seen != requests {
            self.seen = requests;
            self.wanted = Some(requests);
        }
        if self
            .worker
            .as_ref()
            .is_some_and(|(_, worker)| worker.is_finished())
        {
            let (request, worker) = self.worker.take().expect("finished follow-up worker");
            let last = worker.join().unwrap_or(Last::Failed);
            self.retry.reaped(last, now);
            if last == Last::Ended && self.wanted == Some(request) {
                self.wanted = None;
            }
        }
        if self.worker.is_some()
            || self.wanted.is_none()
            || self
                .retry
                .remaining(now)
                .is_some_and(|pause| !pause.is_zero())
        {
            return Ok(());
        }
        let request = self.wanted.expect("wanted follow-up");
        self.retry.started();
        match start() {
            Ok(worker) => self.worker = Some((request, worker)),
            Err(error) => {
                // The request remains wanted. A failed start spent no effect attempt.
                self.retry.reaped(Last::Failed, now);
                return Err(error.to_string());
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_cli::macos_doctor::InitialSetupWithheldStopTest;
    use aterm_secrets::converge::FIRST_RETRY_MS;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    struct Owned(Coordinator);

    impl std::ops::Deref for Owned {
        type Target = Coordinator;
        fn deref(&self) -> &Coordinator {
            &self.0
        }
    }

    impl std::ops::DerefMut for Owned {
        fn deref_mut(&mut self) -> &mut Coordinator {
            &mut self.0
        }
    }

    impl Drop for Owned {
        fn drop(&mut self) {
            if let Some((_, worker)) = self.worker.take() {
                let _ = worker.join();
            }
        }
    }

    struct Release(Option<mpsc::Sender<()>>);

    impl Drop for Release {
        fn drop(&mut self) {
            if let Some(send) = self.0.take() {
                let _ = send.send(());
            }
        }
    }

    fn finished(coordinator: &Coordinator) {
        let deadline = Instant::now() + Duration::from_secs(60);
        while coordinator
            .worker
            .as_ref()
            .is_some_and(|(_, worker)| !worker.is_finished())
        {
            assert!(Instant::now() < deadline, "finite follow-up worker hung");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn start(mut fixture: InitialSetupWithheldStopTest) -> std::io::Result<JoinHandle<Last>> {
        std::thread::Builder::new()
            .name("aterm-test-follow-up".into())
            .spawn(move || {
                super::super::publish_withheld_attempt(
                    fixture.attempt(|| Some(()), |_| false),
                    |_, _| {},
                    |why, _| panic!("unexpected publication refusal: {why}"),
                )
            })
    }

    #[test]
    fn the_grants_real_follow_up_survives_failed_start_and_late_unlock_and_keeps_a_new_request() {
        let root = aterm_tempfile::tempdir().unwrap();
        let mut fixture = InitialSetupWithheldStopTest::unfinished(root.path()).unwrap();
        let publish_restore = fixture.prepare_restore().unwrap();
        let writes = fixture.notification_center_writes();
        assert!(
            !fixture.owed().unwrap(),
            "the Restore has not published its debt"
        );
        let before = std::fs::read(root.path().join("withheld-preferences.json")).ok();
        let requests = Requests::new();
        let mut coordinator = Owned(Coordinator::default());
        coordinator
            .round_with(requests.current(), 0, || panic!("no request"))
            .unwrap();
        requests.request();
        assert!(
            coordinator
                .round_with(requests.current(), 0, || {
                    Err(std::io::Error::other(
                        "injected finite-worker spawn refusal",
                    ))
                })
                .is_err()
        );
        assert!(coordinator.worker.is_none());
        assert_eq!(coordinator.wanted, Some(requests.current()));
        coordinator
            .round_with(requests.current(), FIRST_RETRY_MS - 1, || {
                panic!("a refused start keeps its existing backoff")
            })
            .unwrap();
        coordinator
            .round_with(requests.current(), FIRST_RETRY_MS, || {
                start(fixture.clone())
            })
            .unwrap();
        finished(&coordinator);
        coordinator
            .round_with(requests.current(), FIRST_RETRY_MS, || {
                panic!("held leases wait for the next keeper round")
            })
            .unwrap();
        assert!(coordinator.worker.is_none());
        assert_eq!(coordinator.wanted, Some(requests.current()));
        assert_eq!(fixture.notification_center_writes(), writes);
        assert_eq!(
            std::fs::read(root.path().join("withheld-preferences.json")).ok(),
            before
        );

        // No new grant: the same retained request survives well beyond the removed lock
        // budget, and no held attempt consumed a preference's effect-failure budget.
        publish_restore().unwrap();
        assert!(fixture.owed().unwrap());
        let later = 20 * 60 * 1000;
        let (send, receive) = mpsc::channel();
        coordinator
            .round_with(requests.current(), later, || {
                let mut fixture = fixture.clone();
                std::thread::Builder::new()
                    .name("aterm-test-late-follow-up".into())
                    .spawn(move || {
                        receive
                            .recv_timeout(Duration::from_secs(60))
                            .expect("release the finite worker");
                        super::super::publish_withheld_attempt(
                            fixture.attempt(|| Some(()), |_| false),
                            |_, _| {},
                            |why, _| panic!("unexpected publication refusal: {why}"),
                        )
                    })
            })
            .unwrap();
        let release = Release(Some(send));
        requests.request();
        coordinator
            .round_with(requests.current(), later, || panic!("only one worker"))
            .unwrap();
        assert_eq!(coordinator.wanted, Some(requests.current()));
        drop(release);
        finished(&coordinator);
        // The earlier worker's successful retirement cannot consume the newer request.
        let mut started = false;
        coordinator
            .round_with(requests.current(), later, || {
                started = true;
                start(fixture.clone())
            })
            .unwrap();
        assert!(started);
        finished(&coordinator);
        coordinator
            .round_with(requests.current(), later, || panic!("settled intent"))
            .unwrap();
        assert!(coordinator.worker.is_none());
        assert!(coordinator.wanted.is_none());
        assert!(!fixture.owed().unwrap());
        assert_eq!(fixture.notification_center_writes(), writes + 1);
    }
}
