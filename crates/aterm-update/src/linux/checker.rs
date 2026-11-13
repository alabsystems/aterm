// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The enrolled Linux checker's worker lifetime and host-watchdog recovery.

use super::*;
use crate::checker_watch::{CheckerPhase, CheckerWatch, WATCH};
use std::sync::{Arc, Mutex};

struct Args {
    current_build: u64,
    provider: crate::CheckSettingsProvider,
    notify: Mutex<Option<crate::HealthNotify>>,
    on_checked: Mutex<Option<crate::CheckedNotify>>,
}

static ARGS: OnceLock<Arc<Args>> = OnceLock::new();

pub(super) fn start(
    current_build: u64,
    provider: crate::CheckSettingsProvider,
    notify: Option<crate::HealthNotify>,
    on_checked: Option<crate::CheckedNotify>,
) {
    if !crate::automatic() || !context().is_ok_and(enrolled_in) {
        return;
    }
    let args = Arc::new(Args {
        current_build,
        provider,
        notify: Mutex::new(notify),
        on_checked: Mutex::new(on_checked),
    });
    if ARGS.set(Arc::clone(&args)).is_err() {
        return;
    }
    let generation = WATCH.register(crate::linux_notice::PASS_SECS);
    launch(&WATCH, generation, || spawn(generation, args));
}

pub(crate) fn respawn(stalled: u64) -> Option<u64> {
    let args = Arc::clone(ARGS.get()?);
    let generation = WATCH.supersede(stalled)?;
    launch(&WATCH, generation, || spawn(generation, args)).then_some(generation)
}

/// A failed creation remains visible to the host and retains arguments for retry.
fn launch(
    watch: &CheckerWatch,
    generation: u64,
    spawn: impl FnOnce() -> std::io::Result<()>,
) -> bool {
    match spawn() {
        Ok(()) => true,
        Err(error) => {
            watch.unstarted(generation);
            aterm_log::warn!("Linux update checker could not start: {error}");
            false
        }
    }
}

fn spawn(generation: u64, args: Arc<Args>) -> std::io::Result<()> {
    std::thread::Builder::new()
        .name("aterm-linux-update".into())
        .spawn(move || {
            let Some(_worker) = WATCH.worker(generation) else {
                return;
            };
            let started = now().map_or_else(
                |_| String::new(),
                |unix| aterm_types::rfc3339::format_rfc3339(unix as u64),
            );
            let mut reporter = crate::linux_notice::Reporter::new(started);
            let wake_seen = std::cell::Cell::new(crate::check_wake::SYSTEM_WOKE.posts());
            run(
                &WATCH,
                generation,
                |trigger| {
                    let result = check_recording_owned_in(
                        context(),
                        args.current_build,
                        &args.provider,
                        trigger,
                        Some((&WATCH, generation)),
                    );
                    if !WATCH.is_current(generation) {
                        return false;
                    }
                    let completed = result.2;
                    let on_checked = args
                        .on_checked
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    let notify = args
                        .notify
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    report_pass(
                        &mut reporter,
                        context(),
                        result,
                        on_checked.as_ref(),
                        notify.as_ref(),
                    );
                    completed
                },
                || {
                    let woke = matches!(
                        crate::check_wake::sleep_watching_for_wake(
                            &crate::check_wake::SYSTEM_WOKE,
                            wake_seen.get(),
                            Duration::from_secs(crate::linux_notice::PASS_SECS),
                            || WATCH.is_current(generation),
                        ),
                        crate::check_wake::Waited::Woke(_)
                    );
                    if woke {
                        wake_seen.set(crate::check_wake::SYSTEM_WOKE.posts());
                    }
                    woke
                },
                || aterm_log::warn!("Linux update check panicked; retrying after the normal pause"),
            );
        })
        .map(drop)
}

fn run(
    watch: &CheckerWatch,
    generation: u64,
    mut pass: impl FnMut(CheckTrigger) -> bool,
    mut pause: impl FnMut() -> bool,
    mut panicked: impl FnMut(),
) {
    let mut trigger = CheckTrigger::Scheduled;
    loop {
        if !watch.beat(generation, CheckerPhase::Checking) {
            return;
        }
        let completed =
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| pass(trigger))) {
                Ok(completed) => completed,
                Err(_) => {
                    panicked();
                    false
                }
            };
        if completed {
            watch.note_check(generation);
        }
        if !watch.beat(generation, CheckerPhase::Waiting) {
            return;
        }
        if pause() {
            trigger = CheckTrigger::NetworkMoved;
        } else if completed {
            trigger = CheckTrigger::Scheduled;
        }
    }
}

#[cfg(test)]
#[path = "checker_tests.rs"]
mod tests;
