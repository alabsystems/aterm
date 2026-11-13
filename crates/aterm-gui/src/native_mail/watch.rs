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
#[path = "watch_tests.rs"]
mod tests;
