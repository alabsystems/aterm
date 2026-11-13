// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use aterm_json::json;

const HANG: Duration = Duration::from_secs(60);

struct Mailbox(mpsc::Sender<String>, Arc<AtomicU64>);
impl aterm_mail::proton::ApiTransport for Mailbox {
    fn request(
        &mut self,
        method: aterm_http::Method,
        path: &str,
        _: Option<aterm_json::Value>,
    ) -> Result<aterm_mail::proton::ApiResponse, aterm_mail::proton::ApiError> {
        assert_eq!(method, aterm_http::Method::Get);
        assert!(path.starts_with("/mail/v4/messages?"));
        self.0.send(path.to_owned()).unwrap();
        let revision = self.1.load(Ordering::Acquire);
        let id = if revision == 0 {
            "new-mail".to_owned()
        } else {
            format!("new-mail-{revision}")
        };
        Ok(aterm_mail::proton::ApiResponse {
            status: 200,
            retry_after: None,
            body: aterm_json::to_vec(
                &json!({"Code":1000,"Messages":[{"ID":id,"Subject":"New message","Unread":1}]}),
            )
            .unwrap(),
        })
    }
    fn renew(&mut self) -> Result<(), aterm_mail::proton::ApiError> {
        panic!("authenticated fixture should not renew")
    }
}

fn account() -> (Account, mpsc::Receiver<String>) {
    let (account, calls, _) = versioned_account();
    (account, calls)
}

fn versioned_account() -> (Account, mpsc::Receiver<String>, Arc<AtomicU64>) {
    let (calls, received) = mpsc::channel();
    let revision = Arc::new(AtomicU64::new(0));
    (
        Account::Proton(Arc::new(Mutex::new(
            aterm_mail::proton::Proton::with_transport(Box::new(Mailbox(calls, revision.clone()))),
        ))),
        received,
        revision,
    )
}

#[test]
fn another_mail_arrival_updates_the_running_observer_without_a_command() {
    let (updates, received) = mpsc::sync_channel(1);
    let mut watcher = Watcher::start(updates).unwrap();
    let finished = watcher.finished.take().unwrap();
    let (account, _calls, revision) = versioned_account();
    watcher.control().watch(1, "0".into(), &account, Vec::new());
    let (_, first) = received.recv_timeout(HANG).unwrap();
    let Work::Observed { result, .. } = first else {
        panic!("expected initial mail")
    };
    assert_eq!(result.unwrap()[0].id, "new-mail");
    revision.store(1, Ordering::Release);
    let (_, next) = received.recv_timeout(HANG).unwrap();
    let Work::Observed { result, .. } = next else {
        panic!("expected newly arrived mail")
    };
    assert_eq!(result.unwrap()[0].id, "new-mail-1");
    drop(watcher);
    finished.recv_timeout(HANG).unwrap();
}

fn occupied() -> (u64, Work) {
    (
        0,
        Work::Observed {
            epoch: 0,
            account: 0,
            folder: String::new(),
            result: Ok(Vec::new()),
        },
    )
}

#[test]
fn concurrent_folder_changes_keep_only_the_newest_configuration() {
    let control = Control::new();
    let (account, _calls) = account();
    let Account::Proton(source) = &account else {
        unreachable!()
    };
    std::thread::scope(|scope| {
        for worker in 0..4 {
            let control = &control;
            let account = &account;
            scope.spawn(move || {
                for change in 0..100 {
                    control.watch(worker, format!("{worker}-{change}"), account, Vec::new());
                }
            });
        }
    });
    // Only the account and one pending configuration retain this session.
    assert_eq!(Arc::strong_count(source), 2);
    let Next::Config(config) = control.next(Some(Instant::now())) else {
        panic!("the newest selection must be ready")
    };
    assert_eq!(config.epoch, 400);
    assert!(control.current(config.epoch));
    assert!(matches!(control.next(Some(Instant::now())), Next::TimedOut));
    drop(config);
    assert_eq!(Arc::strong_count(source), 1);
    control.watch(0, "discard-on-stop".into(), &account, Vec::new());
    control.stop_watching();
    assert!(matches!(control.next(None), Next::Stopped));
    assert_eq!(Arc::strong_count(source), 1);
}

#[test]
fn closing_an_idle_watcher_wakes_it_without_a_configuration() {
    let (updates, _received) = mpsc::sync_channel(1);
    let mut watcher = Watcher::start(updates).unwrap();
    let finished = watcher.finished.take().unwrap();
    drop(watcher);
    finished.recv_timeout(HANG).unwrap();
}

#[test]
fn changing_folder_cancels_an_observation_even_when_updates_are_full() {
    let (updates, received) = mpsc::sync_channel(1);
    updates.send(occupied()).unwrap();
    let mut watcher = Watcher::start(updates).unwrap();
    let finished = watcher.finished.take().unwrap();
    let (account, calls) = account();
    let control = watcher.control();
    control.watch(1, "0".into(), &account, Vec::new());
    assert!(calls.recv_timeout(HANG).unwrap().contains("LabelID=0"));
    control.watch(2, "2".into(), &account, Vec::new());
    assert!(calls.recv_timeout(HANG).unwrap().contains("LabelID=2"));
    received.recv_timeout(HANG).unwrap();
    let (_, update) = received.recv_timeout(HANG).unwrap();
    let Work::Observed {
        epoch,
        account,
        folder,
        result,
    } = update
    else {
        panic!("expected a mailbox snapshot")
    };
    assert_eq!((account, folder.as_str()), (2, "2"));
    assert!(control.current(epoch));
    assert_eq!(result.unwrap()[0].id, "new-mail");
    drop(watcher);
    finished.recv_timeout(HANG).unwrap();
}

#[test]
fn closing_mail_stops_a_watcher_with_a_full_update_queue() {
    let (updates, _received) = mpsc::sync_channel(1);
    updates.send(occupied()).unwrap();
    let mut watcher = Watcher::start(updates).unwrap();
    let finished = watcher.finished.take().unwrap();
    let (account, calls) = account();
    watcher.control().watch(1, "0".into(), &account, Vec::new());
    calls.recv_timeout(HANG).unwrap();
    drop(watcher);
    finished.recv_timeout(HANG).unwrap();
}

#[test]
#[ignore = "requires ATERM_MAIL_TEST_ACCOUNT and its authenticated profile"]
fn observes_a_live_proton_account_without_a_foreground_read() {
    let email = std::env::var("ATERM_MAIL_TEST_ACCOUNT").unwrap();
    let mut accounts = Vec::new();
    let connected = super::super::perform(Work::Stored(email), &mut accounts, &mut |_| {}).unwrap();
    let super::super::Reply::Connected {
        account, folder, ..
    } = connected
    else {
        panic!("expected account")
    };
    let (updates, received) = mpsc::sync_channel(1);
    let mut watcher = Watcher::start(updates).unwrap();
    let finished = watcher.finished.take().unwrap();
    let sentinel = Message {
        id: "force-initial-observation".into(),
        from: String::new(),
        to: String::new(),
        subject: String::new(),
        date: String::new(),
        unread: false,
        text: String::new(),
    };
    watcher.control().watch(
        account,
        folder.clone(),
        &accounts[account].1,
        vec![sentinel],
    );
    let (_, update) = received.recv_timeout(HANG).unwrap();
    let Work::Observed {
        account: observed,
        folder: observed_folder,
        result,
        ..
    } = update
    else {
        panic!("expected mailbox observation")
    };
    assert_eq!((observed, observed_folder), (account, folder));
    assert!(result.is_ok());
    drop(watcher);
    finished.recv_timeout(HANG).unwrap();
}

fn stub_message(id: &str) -> Message {
    Message {
        id: id.into(),
        from: String::new(),
        to: String::new(),
        subject: id.into(),
        date: String::new(),
        unread: true,
        text: String::new(),
    }
}

struct RecyclingIdleSource {
    opens: usize,
    stop: Arc<AtomicBool>,
    opened: mpsc::Sender<usize>,
}

struct RecyclingIdleSession {
    connection: usize,
    stop: Arc<AtomicBool>,
}

impl IdleFixtureSource for RecyclingIdleSource {
    fn open(&mut self) -> Result<Box<dyn IdleFixtureSession>, Failure> {
        self.opens += 1;
        let _ = self.opened.send(self.opens);
        Ok(Box::new(RecyclingIdleSession {
            connection: self.opens,
            stop: self.stop.clone(),
        }))
    }
}

impl IdleFixtureSession for RecyclingIdleSession {
    fn messages(&mut self, _limit: usize) -> Result<Vec<Message>, String> {
        Ok(if self.connection == 1 {
            vec![stub_message("first")]
        } else {
            vec![stub_message("first"), stub_message("after-recycle")]
        })
    }

    fn wait(&mut self, _within: Duration) -> Result<bool, String> {
        if self.connection == 1 {
            return Err("IMAP connection closed".into());
        }
        while !self.stop.load(Ordering::Acquire) {
            std::thread::sleep(Duration::from_millis(5));
        }
        Err("stopped".into())
    }
}

#[test]
fn idle_disconnect_reconnects_immediately_without_publishing_sync_waiting() {
    let (updates, received) = mpsc::sync_channel(4);
    let mut watcher = Watcher::start(updates).unwrap();
    let finished = watcher.finished.take().unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let (opened_tx, opened_rx) = mpsc::channel();
    let source = Arc::new(Mutex::new(RecyclingIdleSource {
        opens: 0,
        stop: stop.clone(),
        opened: opened_tx,
    }));
    watcher
        .control()
        .watch_idle(1, "INBOX".into(), source, Vec::new());

    assert_eq!(opened_rx.recv_timeout(HANG).unwrap(), 1);
    let (_, first) = received.recv_timeout(HANG).unwrap();
    let Work::Observed { result, .. } = first else {
        panic!("expected initial observation")
    };
    assert_eq!(result.unwrap(), vec![stub_message("first")]);

    assert_eq!(opened_rx.recv_timeout(HANG).unwrap(), 2);
    let (_, second) = received.recv_timeout(HANG).unwrap();
    let Work::Observed { result, .. } = second else {
        panic!("expected silent reconnect observation")
    };
    assert_eq!(
        result.unwrap(),
        vec![stub_message("first"), stub_message("after-recycle")]
    );
    assert!(received.try_recv().is_err());

    stop.store(true, Ordering::Release);
    drop(watcher);
    finished.recv_timeout(HANG).unwrap();
}

struct AlwaysDroppingIdleSource {
    opens: usize,
    opened: mpsc::Sender<usize>,
}

struct AlwaysDroppingIdleSession;

impl IdleFixtureSource for AlwaysDroppingIdleSource {
    fn open(&mut self) -> Result<Box<dyn IdleFixtureSession>, Failure> {
        self.opens += 1;
        let _ = self.opened.send(self.opens);
        Ok(Box::new(AlwaysDroppingIdleSession))
    }
}

impl IdleFixtureSession for AlwaysDroppingIdleSession {
    fn messages(&mut self, _limit: usize) -> Result<Vec<Message>, String> {
        Ok(vec![stub_message("steady")])
    }

    fn wait(&mut self, _within: Duration) -> Result<bool, String> {
        Err("IDLE rejected".into())
    }
}

#[test]
fn consecutive_idle_drops_back_off_instead_of_spinning() {
    let (updates, received) = mpsc::sync_channel(4);
    let mut watcher = Watcher::start(updates).unwrap();
    let finished = watcher.finished.take().unwrap();
    let (opened_tx, opened_rx) = mpsc::channel();
    let source = Arc::new(Mutex::new(AlwaysDroppingIdleSource {
        opens: 0,
        opened: opened_tx,
    }));
    watcher
        .control()
        .watch_idle(1, "INBOX".into(), source, Vec::new());

    assert_eq!(opened_rx.recv_timeout(HANG).unwrap(), 1);
    let (_, first) = received.recv_timeout(HANG).unwrap();
    let Work::Observed { result, .. } = first else {
        panic!("expected initial observation")
    };
    assert!(result.is_ok());

    // First IDLE drop reconnects immediately; second enters cancellable backoff.
    assert_eq!(opened_rx.recv_timeout(HANG).unwrap(), 2);
    assert!(opened_rx.recv_timeout(Duration::from_millis(100)).is_err());

    drop(watcher);
    finished.recv_timeout(HANG).unwrap();
}
