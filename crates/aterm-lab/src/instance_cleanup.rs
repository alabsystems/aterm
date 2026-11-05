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
}

impl StopRecord {
    pub(super) fn new(world: &Path, failed: Arc<AtomicBool>, pid: i32) -> Self {
        Self {
            result: None,
            world: world.to_owned(),
            failed,
            pid,
            evidence: None,
        }
    }

    pub(super) fn attempt(
        &mut self,
        run: impl FnOnce() -> Result<(), String>,
    ) -> Result<(), String> {
        if let Some(result) = &self.result {
            return result.clone();
        }
        let result = run();
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
mod tests {
    use super::*;
    use crate::instance::{CONFIG_OFF, World};

    #[test]
    fn failed_stop_is_recorded_once_and_preserves_world_and_private_evidence() {
        let mut world = World::new(CONFIG_OFF).unwrap();
        let path = world.dir.clone();
        let mut failed = StopRecord::new(&path, world.cleanup_failed.clone(), 4242);
        assert!(
            failed
                .attempt(|| Err("registration unknown".into()))
                .is_err()
        );
        let evidence = failed.evidence.clone().unwrap();
        let result = aterm_uds::private_file::read_private(&evidence, "result.txt", 4096).unwrap();
        assert!(
            std::str::from_utf8(&result)
                .unwrap()
                .contains("state=unknown\nerror=registration unknown")
        );
        assert_eq!(
            failed.attempt(|| panic!("no repeated cleanup timeout")),
            Err("registration unknown".into())
        );
        let mut other = StopRecord::new(&path, world.cleanup_failed.clone(), 4343);
        other.attempt(|| Ok(())).unwrap();
        world.release();
        drop(world);
        assert!(
            path.is_dir(),
            "another successful stop must not discard failed evidence"
        );
        assert_eq!(
            aterm_uds::private_file::read_private(&evidence, "result.txt", 4096).unwrap(),
            result
        );
        std::fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn successful_stop_is_idempotent_and_allows_normal_world_removal() {
        let world = World::new(CONFIG_OFF).unwrap();
        let path = world.dir.clone();
        let mut stopped = StopRecord::new(&path, world.cleanup_failed.clone(), 4242);
        stopped.attempt(|| Ok(())).unwrap();
        stopped
            .attempt(|| panic!("completed stop is not rerun by Drop"))
            .unwrap();
        assert!(stopped.evidence.is_none());
        drop(world);
        assert!(!path.exists());
    }

    #[test]
    fn cleanup_does_not_replace_primary_failure_or_signal() {
        let primary = Fail {
            msg: Some("work failed".into()),
            code: 143,
            stopped: Some(15),
        };
        let combined = after_stop::<()>(Err(primary), Err("cleanup unknown".into())).unwrap_err();
        assert_eq!(combined.code, 143);
        assert_eq!(combined.stopped, Some(15));
        let message = combined.msg.unwrap();
        assert!(message.contains("work failed") && message.contains("cleanup unknown"));
        assert!(after_stop(Ok(42), Err("cleanup unknown".into())).is_err());
        assert_eq!(after_stop(Ok(42), Ok(())).unwrap(), 42);
    }

    #[test]
    fn keeper_absence_does_not_close_a_live_unknown_or_unidentified_gui() {
        use aterm_uds::process::ProcessPresence::{Absent, Exited, Present, Unknown};
        assert!(!super::super::gui_closed(true, true, Present).unwrap());
        assert!(!super::super::gui_closed(true, true, Exited).unwrap());
        assert!(super::super::gui_closed(true, true, Unknown).is_err());
        assert!(super::super::gui_closed(true, false, Absent).is_err());
        assert!(super::super::gui_closed(true, true, Absent).unwrap());
        assert!(super::super::gui_closed(false, false, Unknown).unwrap());
    }
}
