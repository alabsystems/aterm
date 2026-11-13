// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::cell::Cell;
use std::marker::PhantomData;
use std::rc::Rc;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Report {
    pub(crate) attempts: usize,
    pub(crate) terms: usize,
    pub(crate) kills: usize,
    pub(crate) violations: usize,
}
#[derive(Clone, Copy)]
struct Check {
    pid: u32,
    report: Report,
}
thread_local! { static CHECK: Cell<Option<Check>> = const { Cell::new(None) }; }

pub(crate) struct Scope {
    previous: Option<Check>,
    _same_thread: PhantomData<Rc<()>>,
}
impl Scope {
    pub(crate) fn arm(pid: u32) -> Self {
        let previous = CHECK.with(|c| {
            c.replace(Some(Check {
                pid,
                report: Report::default(),
            }))
        });
        Self {
            previous,
            _same_thread: PhantomData,
        }
    }
    pub(crate) fn report(&self) -> Report {
        CHECK.with(|c| c.get().expect("armed custody check").report)
    }
}
impl Drop for Scope {
    fn drop(&mut self) {
        CHECK.with(|c| c.set(self.previous));
    }
}
pub(super) fn before_signal(pid: u32, sig: i32) -> std::io::Result<()> {
    CHECK.with(|c| {
        let Some(mut check) = c.get() else {
            return Ok(());
        };
        check.report.attempts += 1;
        let valid = check.pid == pid && super::exited(pid, false).is_ok();
        if valid {
            check.report.terms += usize::from(sig == super::SIGTERM);
            check.report.kills += usize::from(sig == super::SIGKILL);
        } else {
            check.report.violations += 1;
        }
        c.set(Some(check));
        if valid {
            Ok(())
        } else {
            Err(std::io::Error::other(
                "custody fixture refused a signal without its unreaped child",
            ))
        }
    })
}
