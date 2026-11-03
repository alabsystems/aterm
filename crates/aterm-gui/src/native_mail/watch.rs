// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//! One cancellable inbox observer per Mail view, independent of foreground requests.

use super::{Account, Message, StandardAccount, Work};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::time::{Duration, Instant};

#[cfg(test)]
trait IdleFixtureSource: Send {
    fn open(&mut self) -> Result<Box<dyn IdleFixtureSession>, Failure>;
}

#[cfg(test)]
trait IdleFixtureSession: Send {
    fn supports_idle(&self) -> bool {
        true
    }
    fn messages(&mut self, limit: usize) -> Result<Vec<Message>, String>;
    fn wait(&mut self, within: Duration) -> Result<bool, String>;
}

enum ActiveReader {
    Imap(Box<aterm_mail::imap::Reader>),
    #[cfg(test)]
    Idle(Box<dyn IdleFixtureSession>),
}

impl ActiveReader {
    fn supports_idle(&self) -> bool {
        match self {
            Self::Imap(reader) => reader.supports_idle(),
            #[cfg(test)]
            Self::Idle(reader) => reader.supports_idle(),
        }
    }

    fn messages(&mut self, limit: usize) -> Result<Vec<Message>, String> {
        match self {
            Self::Imap(reader) => reader.messages(limit),
            #[cfg(test)]
            Self::Idle(reader) => reader.messages(limit),
        }
    }

    fn wait(&mut self, within: Duration) -> Result<bool, String> {
        match self {
            Self::Imap(reader) => reader.wait(within),
            #[cfg(test)]
            Self::Idle(reader) => reader.wait(within),
        }
    }
}

#[derive(Clone)]
enum Source {
    Proton(Arc<Mutex<aterm_mail::proton::Proton>>),
    Standard(Arc<Mutex<StandardAccount>>),
    #[cfg(test)]
    Idle(Arc<Mutex<dyn IdleFixtureSource>>),
}

struct Config {
    epoch: u64,
    account: usize,
    folder: String,
    source: Source,
    baseline: Vec<Message>,
}

pub(super) struct Control {
    pending_selection: Mutex<Option<Config>>,
    changed: Condvar,
    epoch: AtomicU64,
    stopped: AtomicBool,
    interrupt: Mutex<Option<aterm_http::stream::Interrupt>>,
}

pub(super) struct Watcher {
    control: Arc<Control>,
    #[cfg(test)]
    finished: Option<mpsc::Receiver<()>>,
}

impl Watcher {
    pub(super) fn start(updates: mpsc::SyncSender<(u64, Work)>) -> Result<Self, String> {
        let control = Arc::new(Control::new());
        let background = control.clone();
        #[cfg(test)]
        let (done, finished) = mpsc::channel();
        crate::qos::spawn_lane_thread(
            "aterm-mail-watch",
            crate::qos::Role::Background,
            move || {
                let mut next = latest(&background);
                while let Some(config) = next {
                    next = observe(config, &background, &updates);
                }
                #[cfg(test)]
                let _ = done.send(());
            },
        )
        .map_err(|error| error.to_string())?;
        Ok(Self {
            control,
            #[cfg(test)]
            finished: Some(finished),
        })
    }

    pub(super) fn control(&self) -> Arc<Control> {
        self.control.clone()
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        self.control.stop_watching();
    }
}

enum Next {
    Config(Config),
    TimedOut,
    Stopped,
}

impl Control {
    fn new() -> Self {
        Self {
            pending_selection: Mutex::new(None),
            changed: Condvar::new(),
            epoch: AtomicU64::new(0),
            stopped: AtomicBool::new(false),
            interrupt: Mutex::new(None),
        }
    }

    fn stop_watching(&self) {
        // Share the predicate lock with waiters, so a stop cannot lose its wake.
        let pending = self.pending_selection.lock();
        self.stopped.store(true, Ordering::Release);
        self.cancel_socket();
        drop(pending);
        self.changed.notify_all();
    }

    fn next(&self, end: Option<Instant>) -> Next {
        let Ok(mut pending) = self.pending_selection.lock() else {
            return Next::Stopped;
        };
        loop {
            if self.stopped.load(Ordering::Acquire) {
                let abandoned = pending.take();
                drop(pending);
                // Account destruction, including browser cleanup, stays outside the lock.
                drop(abandoned);
                return Next::Stopped;
            }
            if let Some(config) = pending.take() {
                return Next::Config(config);
            }
            pending = if let Some(end) = end {
                let left = end.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    return Next::TimedOut;
                }
                match self.changed.wait_timeout(pending, left) {
                    Ok((pending, _)) => pending,
                    Err(_) => return Next::Stopped,
                }
            } else {
                match self.changed.wait(pending) {
                    Ok(pending) => pending,
                    Err(_) => return Next::Stopped,
                }
            };
        }
    }

    pub(super) fn current(&self, epoch: u64) -> bool {
        !self.stopped.load(Ordering::Acquire) && self.epoch.load(Ordering::Acquire) == epoch
    }

    pub(super) fn watch(
        &self,
        account: usize,
        folder: String,
        source: &Account,
        baseline: Vec<Message>,
    ) {
        let source = match source {
            Account::Proton(source) => Source::Proton(source.clone()),
            Account::Standard(source) => Source::Standard(source.clone()),
        };
        self.watch_source(account, folder, source, baseline);
    }

    #[cfg(test)]
    fn watch_idle(
        &self,
        account: usize,
        folder: String,
        source: Arc<Mutex<dyn IdleFixtureSource>>,
        baseline: Vec<Message>,
    ) {
        self.watch_source(account, folder, Source::Idle(source), baseline);
    }

    fn watch_source(&self, account: usize, folder: String, source: Source, baseline: Vec<Message>) {
        let Ok(mut pending) = self.pending_selection.lock() else {
            return;
        };
        if self.stopped.load(Ordering::Acquire) {
            return;
        }
        let epoch = self.epoch.fetch_add(1, Ordering::AcqRel).wrapping_add(1);
        let old = pending.replace(Config {
            epoch,
            account,
            folder,
            source,
            baseline,
        });
        // The new configuration cannot be adopted until its predecessor's socket is cancelled.
        self.cancel_socket();
        drop(pending);
        drop(old);
        self.changed.notify_one();
    }

    fn cancel_socket(&self) {
        if let Ok(mut interrupt) = self.interrupt.lock()
            && let Some(interrupt) = interrupt.take()
        {
            interrupt.interrupt();
        }
    }

    fn install(&self, epoch: u64, reader: &aterm_mail::imap::Reader) -> Result<(), Failure> {
        let handle = reader.interruptor().map_err(Failure::retry)?;
        let mut slot = self
            .interrupt
            .lock()
            .map_err(|_| Failure::retry("Mail cancellation lock failed"))?;
        if self.current(epoch) {
            *slot = Some(handle);
        } else {
            handle.interrupt();
        }
        Ok(())
    }
}

fn latest(control: &Control) -> Option<Config> {
    match control.next(None) {
        Next::Config(config) => Some(config),
        Next::Stopped | Next::TimedOut => None,
    }
}

struct Failure {
    reason: String,
    permanent: bool,
    quiet: bool,
}
impl Failure {
    fn busy() -> Self {
        Self {
            reason: String::new(),
            permanent: false,
            quiet: true,
        }
    }

    fn retry(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
            permanent: false,
            quiet: false,
        }
    }
}

fn publish(
    config: &Config,
    control: &Control,
    updates: &mpsc::SyncSender<(u64, Work)>,
    result: Result<Vec<Message>, String>,
) -> bool {
    let mut work = (
        0,
        Work::Observed {
            epoch: config.epoch,
            account: config.account,
            folder: config.folder.clone(),
            result,
        },
    );
    while control.current(config.epoch) {
        match updates.try_send(work) {
            Ok(()) => return true,
            Err(mpsc::TrySendError::Disconnected(_)) => return false,
            Err(mpsc::TrySendError::Full(pending)) => work = pending,
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    false
}

fn observe(
    mut config: Config,
    control: &Control,
    updates: &mpsc::SyncSender<(u64, Work)>,
) -> Option<Config> {
    let mut reader = None;
    let mut generation = None;
    let mut failures = 0u32;
    let mut idle_drops = 0u32;
    let mut needs_read = true;
    loop {
        if !control.current(config.epoch) {
            return latest(control);
        }
        let result = if needs_read {
            read(&config, control, &mut reader, &mut generation)
        } else {
            Ok(config.baseline.clone())
        };
        if !control.current(config.epoch) {
            return latest(control);
        }
        let can_idle = result.is_ok();
        let wait = match result {
            Ok(messages) => {
                if (messages != config.baseline || failures != 0)
                    && !publish(&config, control, updates, Ok(messages.clone()))
                {
                    return if control.current(config.epoch) {
                        None
                    } else {
                        latest(control)
                    };
                }
                config.baseline = messages;
                failures = 0;
                Duration::from_secs(2)
            }
            Err(error) => {
                if error.quiet {
                    Duration::from_secs(2)
                } else {
                    reader = None;
                    idle_drops = 0;
                    control.cancel_socket();
                    if !publish(&config, control, updates, Err(error.reason)) {
                        return if control.current(config.epoch) {
                            None
                        } else {
                            latest(control)
                        };
                    }
                    if error.permanent {
                        return latest(control);
                    }
                    failures = failures.saturating_add(1);
                    super::retry_interval(failures)
                }
            }
        };
        if can_idle && let Some(active) = reader.as_mut().filter(|reader| reader.supports_idle()) {
            match active.wait(Duration::from_secs(30)) {
                Ok(changed) => {
                    idle_drops = 0;
                    needs_read = changed;
                }
                Err(_) => {
                    reader = None;
                    control.cancel_socket();
                    needs_read = true;
                    idle_drops = idle_drops.saturating_add(1);
                    if idle_drops > 1 {
                        let backoff = super::retry_interval(idle_drops - 1);
                        match control.next(Instant::now().checked_add(backoff)) {
                            Next::Config(next) => return Some(next),
                            Next::Stopped => return None,
                            Next::TimedOut => {}
                        }
                    }
                }
            }
        } else {
            needs_read = true;
            match control.next(Instant::now().checked_add(wait)) {
                Next::Config(next) => return Some(next),
                Next::Stopped => return None,
                Next::TimedOut => {}
            }
        }
    }
}

fn read(
    config: &Config,
    control: &Control,
    reader: &mut Option<ActiveReader>,
    generation: &mut Option<u64>,
) -> Result<Vec<Message>, Failure> {
    let end = Instant::now() + Duration::from_secs(10);
    match &config.source {
        Source::Proton(source) => {
            let mut source = loop {
                if !control.current(config.epoch) || Instant::now() >= end {
                    return Err(Failure::busy());
                }
                match source.try_lock() {
                    Ok(source) => break source,
                    Err(std::sync::TryLockError::Poisoned(_)) => {
                        return Err(Failure::retry("Mail account lock failed"));
                    }
                    Err(std::sync::TryLockError::WouldBlock) => {
                        std::thread::sleep(Duration::from_millis(10))
                    }
                }
            };
            source
                .run_until(end, |source| {
                    source.with_verification(
                        |source| source.observe_messages(&config.folder, 50, &config.baseline),
                        |_| Err("Mail session verification unavailable".into()),
                    )
                })
                .map_err(|reason| Failure {
                    reason,
                    quiet: false,
                    permanent: source.last_failure().is_some_and(|failure| {
                        failure.recovery() != aterm_mail::proton_error::Recovery::RetryRead
                    }),
                })
        }
        Source::Standard(source) => {
            let (imap, current) = {
                let mut source = loop {
                    if !control.current(config.epoch) || Instant::now() >= end {
                        return Err(Failure::busy());
                    }
                    match source.try_lock() {
                        Ok(source) => break source,
                        Err(std::sync::TryLockError::Poisoned(_)) => {
                            return Err(Failure::retry("Mail account lock failed"));
                        }
                        Err(std::sync::TryLockError::WouldBlock) => {
                            std::thread::sleep(Duration::from_millis(10))
                        }
                    }
                };
                source.refresh().map_err(|error| Failure {
                    permanent: !error.retryable(),
                    reason: error.to_string(),
                    quiet: false,
                })?;
                (source.imap.clone(), source.generation)
            };
            if *generation != Some(current) {
                *reader = None;
                control.cancel_socket();
            }
            if !control.current(config.epoch) {
                return Err(Failure::busy());
            }
            if reader.is_none() {
                let active = imap
                    .open_with_connect_timeout(&config.folder, Duration::from_secs(10))
                    .map_err(|error| match error {
                        aterm_mail::imap::ConnectError::Authentication => Failure {
                            reason: "Mail authentication was refused".into(),
                            permanent: true,
                            quiet: false,
                        },
                        aterm_mail::imap::ConnectError::Other(error) => Failure::retry(error),
                    })?;
                control.install(config.epoch, &active)?;
                *reader = Some(ActiveReader::Imap(Box::new(active)));
                *generation = Some(current);
            }
            reader
                .as_mut()
                .ok_or_else(|| Failure::retry("Mail observer unavailable"))?
                .messages(50)
                .map_err(Failure::retry)
        }
        #[cfg(test)]
        Source::Idle(source) => {
            if !control.current(config.epoch) {
                return Err(Failure::busy());
            }
            if reader.is_none() {
                let mut source = source
                    .lock()
                    .map_err(|_| Failure::retry("Mail fixture lock failed"))?;
                *reader = Some(ActiveReader::Idle(source.open()?));
            }
            reader
                .as_mut()
                .ok_or_else(|| Failure::retry("Mail observer unavailable"))?
                .messages(50)
                .map_err(Failure::retry)
        }
    }
}

#[cfg(test)]
mod tests {
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
                aterm_mail::proton::Proton::with_transport(Box::new(Mailbox(
                    calls,
                    revision.clone(),
                ))),
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
        let connected =
            super::super::perform(Work::Stored(email), &mut accounts, &mut |_| {}).unwrap();
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
}
