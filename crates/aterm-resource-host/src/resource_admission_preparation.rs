// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Preparation cannot outlive the scheduling fields that paid for it, even
//! when its coordinator unwinds. This owns a thread, never resource authority.
pub(super) struct Worker<T> {
    thread: Option<std::thread::JoinHandle<T>>,
    #[cfg(all(test, target_os = "macos"))]
    drop_notice: Option<std::sync::mpsc::SyncSender<()>>,
}
impl<T> Worker<T> {
    pub(super) fn new(thread: std::thread::JoinHandle<T>) -> Self {
        Self {
            thread: Some(thread),
            #[cfg(all(test, target_os = "macos"))]
            drop_notice: None,
        }
    }
    #[cfg(all(test, target_os = "macos"))]
    pub(super) fn notify_drop(&mut self, sender: std::sync::mpsc::SyncSender<()>) {
        self.drop_notice = Some(sender);
    }
    pub(super) fn is_finished(&self) -> bool {
        self.thread
            .as_ref()
            .is_none_or(std::thread::JoinHandle::is_finished)
    }
    pub(super) fn join(mut self) -> std::thread::Result<T> {
        self.thread.take().expect("owned worker joins once").join()
    }
}
impl<T> Drop for Worker<T> {
    fn drop(&mut self) {
        if let Some(thread) = self.thread.take() {
            #[cfg(all(test, target_os = "macos"))]
            if let Some(sender) = self.drop_notice.take() {
                let _ = sender.try_send(());
            }
            let _ = thread.join();
        }
    }
}

#[cfg(all(test, target_os = "macos"))]
pub(super) struct Pause {
    entered: std::sync::mpsc::SyncSender<()>,
    release: std::sync::mpsc::Receiver<()>,
    pub(super) drop_notice: std::sync::mpsc::SyncSender<()>,
}
#[cfg(all(test, target_os = "macos"))]
pub(super) struct Paused {
    pub(super) entered: std::sync::mpsc::Receiver<()>,
    pub(super) dropping: std::sync::mpsc::Receiver<()>,
    pub(super) release: std::sync::mpsc::SyncSender<()>,
}
#[cfg(all(test, target_os = "macos"))]
impl Pause {
    pub(super) fn new() -> (Self, Paused) {
        let (send, entered) = std::sync::mpsc::sync_channel(1);
        let (release, wait) = std::sync::mpsc::sync_channel(1);
        let (drop_notice, dropping) = std::sync::mpsc::sync_channel(1);
        (
            Self {
                entered: send,
                release: wait,
                drop_notice,
            },
            Paused {
                entered,
                dropping,
                release,
            },
        )
    }
    pub(super) fn wait(self) -> std::io::Result<()> {
        let _ = self.entered.send(());
        self.release
            .recv_timeout(std::time::Duration::from_secs(60))
            .map_err(|_| std::io::Error::other("controlled preparation was not released"))
    }
}
