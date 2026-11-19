// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Exercise stage settlement with a real owning child wait and a scripted
//! helper observer. The observer cannot run before either sweep or reap, and
//! refusing it must keep the closing callback from claiming completion.

use super::group::{Live, Spawning, WaitChild};
use std::cell::Cell;
use std::io;

struct Observed<'a> {
    child: aterm_uds::spawnfd::Child,
    swept: &'a Cell<bool>,
    reaped: bool,
    finished: bool,
    refuse: bool,
}

impl WaitChild for Observed<'_> {
    fn id(&self) -> u32 {
        self.child.id()
    }
    fn wait(&mut self) -> io::Result<std::process::ExitStatus> {
        assert!(self.swept.get(), "group sweep retains its unreaped leader");
        let status = self.child.wait()?;
        self.reaped = true;
        Ok(status)
    }
    fn finish_tether(&mut self) -> io::Result<()> {
        assert!(self.reaped && self.swept.get());
        self.finished = true;
        if self.refuse {
            Err(io::Error::other("scripted native watcher unreadable"))
        } else {
            Ok(())
        }
    }
}

#[test]
fn stage_closure_follows_sweep_owning_reap_and_successful_helper_retirement() {
    for refuse in [false, true] {
        let swept = Cell::new(false);
        let closed = Cell::new(false);
        let spawning = Spawning::begin();
        let child = aterm_uds::spawnfd::command(aterm_uds::deadline::stand_in(
            std::path::Path::new("/usr/bin/true"),
        ))
        .process_group(0)
        .spawn()
        .unwrap();
        let mut live = Live::enter(child.id());
        drop(spawning);
        let mut observed = Observed {
            child,
            swept: &swept,
            reaped: false,
            finished: false,
            refuse,
        };
        let result = live.reap_stage_sweeping(
            &mut observed,
            true,
            |_| swept.set(true),
            |_| closed.set(true),
        );
        assert!(observed.reaped && observed.finished);
        assert_eq!(closed.get(), !refuse);
        if refuse {
            assert!(
                result
                    .unwrap_err()
                    .to_string()
                    .contains("watcher unreadable")
            );
        } else {
            assert!(result.unwrap().unwrap().success());
        }
        assert!(observed.child.try_wait().unwrap().is_some());
    }
}
