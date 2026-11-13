// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Optional panel reads never hold the name writer. Pending requests coalesce to the
//! newest panel; the panel's press ID rejects results for an alert that has closed.

use aterm_agent::machine_name::file::MachineName;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

pub(super) fn discovery() -> std::io::Result<Sender<u64>> {
    let (send, requests) = channel();
    std::thread::Builder::new()
        .name("aterm-name-discovery".to_owned())
        .spawn(move || {
            run(requests, |press| {
                let available = aterm_secrets::discover::browse_members(Duration::from_secs(2))
                    .map(|members| !members.is_empty())
                    .map_err(|why| why.to_string());
                aterm_objc::run_on_main(move |_| {
                    crate::machine_rename_sheet::deliver_discovery(press, available);
                });
            });
        })?;
    Ok(send)
}

pub(super) struct Diagnostics {
    pub press: u64,
    pub state: PathBuf,
    pub config: PathBuf,
    pub name: MachineName,
}

pub(super) fn diagnostics() -> std::io::Result<Sender<Diagnostics>> {
    let (send, requests) = channel::<Diagnostics>();
    std::thread::Builder::new()
        .name("aterm-name-diagnostics".to_owned())
        .spawn(move || {
            run(requests, |request| {
                let status = super::naming::NamingStatus::read(
                    &request.state,
                    &request.config,
                    &request.name,
                    &super::MacSystemNames,
                );
                aterm_objc::run_on_main(move |_| {
                    crate::machine_rename_sheet::deliver_diagnostics(request.press, Ok(status));
                });
            });
        })?;
    Ok(send)
}

/// One replaceable request and one wake token, independent of how long a read takes.
pub(super) struct Sender<T> {
    pending: Arc<Mutex<Option<T>>>,
    wake: mpsc::SyncSender<()>,
}

struct Receiver<T> {
    pending: Arc<Mutex<Option<T>>>,
    wake: mpsc::Receiver<()>,
}

fn channel<T>() -> (Sender<T>, Receiver<T>) {
    let pending = Arc::new(Mutex::new(None));
    let (send, receive) = mpsc::sync_channel(1);
    (
        Sender {
            pending: pending.clone(),
            wake: send,
        },
        Receiver {
            pending,
            wake: receive,
        },
    )
}

impl<T> Sender<T> {
    pub(super) fn send(&self, request: T) -> Result<(), mpsc::SendError<()>> {
        let previous = self
            .pending
            .lock()
            .unwrap_or_else(|why| why.into_inner())
            .replace(request);
        drop(previous);
        match self.wake.try_send(()) {
            Ok(()) | Err(mpsc::TrySendError::Full(())) => Ok(()),
            Err(mpsc::TrySendError::Disconnected(())) => {
                let abandoned = self
                    .pending
                    .lock()
                    .unwrap_or_else(|why| why.into_inner())
                    .take();
                drop(abandoned);
                Err(mpsc::SendError(()))
            }
        }
    }
}

fn run<T>(requests: Receiver<T>, mut read: impl FnMut(T)) {
    while requests.wake.recv().is_ok() {
        let latest = requests
            .pending
            .lock()
            .unwrap_or_else(|why| why.into_inner())
            .take();
        if let Some(request) = latest {
            read(request);
        }
    }
}

#[cfg(test)]
#[path = "panel_reads_tests.rs"]
mod tests;
