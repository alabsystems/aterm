// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Background holders cancel their own dedicated group. A ledger observer never signals
//! a foreign numeric pid, which could have been reused after its claim was read.

use std::io;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

#[derive(Debug)]
pub(crate) struct Guard {
    stopped: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Guard {
    pub(crate) fn start(ledger: &crate::Ledger, rank: crate::Rank) -> io::Result<Option<Self>> {
        if rank != crate::Rank::Background {
            return Ok(None);
        }
        #[cfg(unix)]
        {
            // SAFETY: getpgrp has no pointer arguments.
            if unsafe { libc::getpgrp() } != std::process::id() as i32 {
                return Err(io::Error::other(
                    "background holder must lead its own process group",
                ));
            }
            let ledger = ledger.clone();
            let stopped = Arc::new(AtomicBool::new(false));
            let done = stopped.clone();
            let thread = std::thread::Builder::new()
                .name("capacity-background".into())
                .spawn(move || {
                    while !done.load(Ordering::Acquire) {
                        // A failed ledger read ends repeatable work too: it supplies no authority
                        // to keep background work running beside a release we cannot observe.
                        if ledger.list().map_or(true, |rows| {
                            rows.iter().any(|r| r.need.rank == crate::Rank::Release)
                        }) {
                            let pid = std::process::id() as i32;
                            // SAFETY: this live process owns its pid throughout this call. It
                            // authorized cancellation at admission only as a dedicated group
                            // leader; recheck that it has not joined another group since then.
                            if unsafe { libc::getpgrp() } == pid {
                                unsafe {
                                    libc::killpg(pid, libc::SIGTERM);
                                }
                            }
                            break;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(20));
                    }
                })?;
            Ok(Some(Self {
                stopped,
                thread: Some(thread),
            }))
        }
        #[cfg(not(unix))]
        {
            let _ = ledger;
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "background groups require Unix",
            ))
        }
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
