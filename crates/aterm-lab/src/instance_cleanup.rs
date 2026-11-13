// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One recorded stop attempt and retained evidence when private-world cleanup is unknown.

use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use crate::Fail;

pub(super) struct StopRecord {
    result: Option<Result<(), String>>,
    world: PathBuf,
    failed: Arc<AtomicBool>,
    pid: i32,
    evidence: Option<PathBuf>,
    pending: Option<String>,
}

impl StopRecord {
    pub(super) fn new(world: &Path, failed: Arc<AtomicBool>, pid: i32) -> Self {
        Self {
            result: None,
            world: world.to_owned(),
            failed,
            pid,
            evidence: None,
            pending: None,
        }
    }

    pub(super) fn require_evidence(&mut self, why: String) {
        self.pending.get_or_insert(why);
    }

    pub(super) fn attempt(
        &mut self,
        run: impl FnOnce() -> Result<(), String>,
    ) -> Result<(), String> {
        if let Some(result) = &self.result {
            return result.clone();
        }
        // Still attempt the original owned cleanup when a wider ownership scope
        // could not be captured. That missing evidence cannot become a clean receipt.
        let result = match (run(), self.pending.take()) {
            (Ok(()), Some(why)) => Err(why),
            (Err(original), Some(why)) => Err(format!("{original}; {why}")),
            (result, None) => result,
        };
        if let Err(error) = &result {
            self.failed.store(true, Ordering::Release);
            let saved = (|| -> Result<PathBuf, String> {
                let dir = aterm_tempfile::Builder::new()
                    .prefix("cleanup-unknown-")
                    .suffix(".noindex")
                    .tempdir_in(&self.world)
                    .map_err(|why| why.to_string())?;
                aterm_uds::private_file::write_private(
                    dir.path(),
                    "result.txt",
                    format!(
                        "aterm-lab-cleanup/1\npid={}\nstate=unknown\nerror={error}\n",
                        self.pid
                    )
                    .as_bytes(),
                )
                .map_err(|why| why.to_string())?;
                Ok(dir.keep())
            })();
            match saved {
                Ok(path) => {
                    eprintln!(
                        "aterm-lab: cleanup unknown for pid {}: {error}; evidence {}",
                        self.pid,
                        path.display()
                    );
                    self.evidence = Some(path);
                }
                Err(why) => eprintln!(
                    "aterm-lab: cleanup unknown for pid {}: {error}; cannot save evidence: {why}; keeping {}",
                    self.pid,
                    self.world.display()
                ),
            }
        }
        self.result = Some(result.clone());
        result
    }
}

/// Preserve the workload's exit/signal evidence while reporting cleanup failure as well.
pub(crate) fn after_stop<T>(work: Result<T, Fail>, cleanup: Result<(), String>) -> Result<T, Fail> {
    match (work, cleanup) {
        (work, Ok(())) => work,
        (Ok(_), Err(error)) => Err(Fail::die(format!("cleanup failed: {error}"))),
        (Err(mut primary), Err(error)) => {
            primary.msg = Some(match primary.msg {
                Some(message) => format!("{message}; cleanup also failed: {error}"),
                None => format!("cleanup also failed: {error}"),
            });
            Err(primary)
        }
    }
}

#[cfg(test)]
#[path = "instance_cleanup_tests.rs"]
mod tests;
