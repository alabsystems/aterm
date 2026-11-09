// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A selected writer's private vault home has no login subscriber of its own.
//! Refresh it under the transport owner without blocking that owner's LAN ticks.

use super::{Stop, client, custody, enabled, owner};
use crate::{Result, generation};
use aterm_secrets::client::Paths;
use std::{
    path::Path,
    sync::Arc,
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const INTERVAL: Duration = Duration::from_secs(30);
const WITHIN: Duration = Duration::from_secs(20);

pub(super) struct Reader {
    stop: Arc<Stop>,
    task: Option<JoinHandle<()>>,
}

impl Reader {
    pub(super) fn start(
        paths: &Paths,
        config: &Path,
        lease: &Arc<owner::Lease>,
        custody: &Arc<custody::Custody>,
    ) -> Result<Option<Self>> {
        if !generation::is_candidate(paths) {
            return Ok(None);
        }
        let logical = generation::logical_paths(paths)?;
        let paths = paths.clone();
        let config = config.to_owned();
        let lease = lease.clone();
        let custody = custody.clone();
        let stop = Arc::new(Stop::default());
        let worker = stop.clone();
        let task = thread::Builder::new()
            .name("commons-vault-reader".into())
            .spawn(move || {
                loop {
                    worker.sleep(INTERVAL);
                    if worker.is_set() {
                        break;
                    }
                    if !enabled(&config).unwrap_or(false)
                        || aterm_secrets::service::subscriber_turned_off(&logical.home)
                        || lease.recheck().is_err()
                        || custody.recheck().is_err()
                    {
                        continue;
                    }
                    let deadline = Instant::now() + WITHIN;
                    if generation::guard_until(&paths, false, deadline).is_err() {
                        continue;
                    }
                    if worker.is_set() {
                        break;
                    }
                    // This member-only pass shares one bound across discovery,
                    // requests and materialization. It neither joins nor mirrors
                    // a vault; failed reads remain retryable on the next pass.
                    let _ = client::sync_reader_until(&paths, deadline);
                }
            })?;
        Ok(Some(Self {
            stop,
            task: Some(task),
        }))
    }
}

impl Drop for Reader {
    fn drop(&mut self) {
        self.stop.set();
        if let Some(task) = self.task.take() {
            let _ = task.join();
        }
    }
}
