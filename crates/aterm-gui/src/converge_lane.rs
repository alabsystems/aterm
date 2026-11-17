// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The converge pass's window lane (docs/DESIGN-init-on-by-default-2026-10-25.md §8.5): a
//! windowed instance starts `aterm init --converge --trigger window` off the event loop at once
//! and every [`crate::secrets_join::MEND_EVERY`] after a settled pass, or at once when this window's network
//! moved ([`crate::network_moved`], docs/DESIGN-fluid-swarm-2026-10-28.md F3c), beside the vault
//! subscriber's mend. It covers a Mac whose subscriber runs a held copy of an older aterm, which
//! an update never restarts onto new code, and it is the trigger that joins an open vault (Local
//! Network is aterm.app's). The first-launch Secrets stage starts one pass of its own as it ends
//! ([`start_after_secrets`]). Standing unfinished work waits for an input or network transition
//! without a maintenance or retry deadline; transient failures use bounded backoff.

#![cfg_attr(not(unix), allow(dead_code))]

static PASS_ENDED: std::sync::OnceLock<fn()> = std::sync::OnceLock::new();

pub(crate) fn on_pass_ended(ended: fn()) {
    let _ = PASS_ENDED.set(ended);
}

#[cfg(unix)]
fn pass_ended() {
    if let Some(ended) = PASS_ENDED.get() {
        ended();
    }
}

/// Whether this process runs the converge pass's window lane, or the Secrets stage's pass: a
/// windowed instance (not `headless`) on a Unix desktop whose first-run setup is not running now
/// (`unified_setup`: the setup's own startup then starts the lane, `crate::start_converge_lane`,
/// and its Secrets stage a pass as it ends), outside a scratch world and CI
/// ([`aterm_types::world::MARKER`], `CI`, as the Commons runtime), `set` saying which of those
/// variables are set.
pub(crate) fn runs(headless: bool, unified_setup: bool, set: &dyn Fn(&str) -> bool) -> bool {
    cfg!(unix) && !headless && !unified_setup && !set(aterm_types::world::MARKER) && !set("CI")
}

/// Whether the environment variable `name` is set.
pub(crate) fn env_set(name: &str) -> bool {
    std::env::var_os(name).is_some()
}

#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LaneWake {
    Network(crate::network_moved::Woke),
    /// An input look called for this work: with what moved the inputs' mark in the looks that
    /// called for work since the snapshot before, as classes, and how many such looks there were.
    Inputs(
        aterm_secrets::converge::RepositoryWork,
        aterm_secrets::converge::MarkMoved,
        u32,
    ),
}

/// Start the lane: a pass at once, then maintenance, transient retry or input movement as its
/// result allows. Each pass is waited for on the lane's own thread (never the event loop) and
/// its exit said once per change. Whether the thread started; never joined.
pub(crate) fn spawn_lane() -> bool {
    #[cfg(unix)]
    {
        let Ok(exe) = std::env::current_exe() else {
            aterm_log::warn!("converge: no path to this aterm, so the window's pass did not start");
            return false;
        };
        let mut moved = crate::network_moved::watch();
        let spawned = crate::qos::spawn_lane_thread(
            "aterm-converge",
            crate::qos::Role::Background,
            move || {
                // Capture the event generation before the first semantic snapshot. A later change
                // remains pending through an active child, without storing its naming target.
                let mut wake = aterm_secrets::client::ConvergenceWake::default();
                let mut inputs = aterm_secrets::client::Paths::live().map_or_else(
                    |_| aterm_secrets::client::ConvergenceInputs::new(Box::new(|_| None)),
                    |paths| {
                        // The lane wakes on its own events and pace: no doorbell.
                        aterm_secrets::client::convergence_inputs(
                            &paths,
                            aterm_secrets::client::ConvergenceRole::Window,
                            None,
                        )
                    },
                );
                let _ = inputs.snapshot();
                let clock = std::time::Instant::now();
                lane(
                    &mut window_pass(&exe),
                    &mut |line| aterm_log::info!("{line}"),
                    &mut |every| until_moved(&mut moved, &mut wake, &mut inputs, every),
                    &mut || u64::try_from(clock.elapsed().as_millis()).unwrap_or(u64::MAX),
                );
            },
        );
        match spawned {
            Ok(_) => true,
            Err(error) => {
                aterm_log::warn!("converge: the window's lane did not start: {error}");
                false
            }
        }
    }
    #[cfg(not(unix))]
    {
        false
    }
}

/// One pass of the lane ([`lane`]): started under its trigger, its work and its start's note, it
/// says how the pass ended.
#[cfg(unix)]
type LanePass<'a> = dyn FnMut(
        &str,
        aterm_secrets::converge::RepositoryWork,
        &aterm_secrets::converge::StartNote,
    ) -> Ended
    + 'a;

/// The lane's rounds: a window pass at once ([`aterm_secrets::converge::WINDOW`]), then, each
/// time `wait` waited out the recheck or bounded retry, or was woken by a move, another;
/// `log` gets a pass's [`ended_line`] once per change of how passes end, and the source of each
/// move that started one ([`moved_line`]). It returns once `wait` says to stop (`None`), which
/// the live lane's never does. A pass whose runner meets the lock another pass holds waits for
/// it and runs, so `pass` returns only once it ran (or failed), and what comes due meanwhile
/// is the next round's. Each pass is handed the note of its start
/// ([`aterm_secrets::converge::StartNote`]): why it runs (the first pass; a timer, by how the
/// pass before it ended; a network move; an input, with what moved the inputs' mark).
#[cfg(unix)]
fn lane(
    pass: &mut LanePass<'_>,
    log: &mut dyn FnMut(&str),
    wait: &mut dyn FnMut(Option<std::time::Duration>) -> Option<LaneWake>,
    now_ms: &mut dyn FnMut() -> u64,
) {
    use aterm_secrets::converge::{Causes, MarkMoved, RepositoryWork, StartNote};
    let mut retry = aterm_secrets::converge::RunnerRetry::default();
    let mut said = None;
    let mut work = RepositoryWork::Refresh;
    let mut note = StartNote {
        causes: Causes::FIRST,
        ..StartNote::default()
    };
    loop {
        retry.started();
        let ended = pass(aterm_secrets::converge::WINDOW, work, &note);
        let ended_ms = now_ms();
        retry.reaped(ended.last(), ended_ms);
        if said.as_ref() != Some(&ended) {
            if let Some(line) = ended_line(&ended) {
                log(&line);
            }
            said = Some(ended.clone());
        }
        let every = (ended != Ended::Standing).then(|| {
            retry
                .remaining(ended_ms)
                .map_or(crate::secrets_join::MEND_EVERY, |pause| {
                    pause.min(crate::secrets_join::MEND_EVERY)
                })
        });
        let (cause, moved) = loop {
            match wait(every) {
                None => return,
                Some(LaneWake::Network(crate::network_moved::Woke::Timer)) => {
                    let cause = match ended {
                        Ended::Standing => continue,
                        Ended::Ran => {
                            work = RepositoryWork::Refresh;
                            Causes::TIMER
                        }
                        Ended::Unfinished => Causes::RETRY_UNFINISHED,
                        Ended::Failed(_) | Ended::Unstarted(_) => Causes::RETRY_FAILED,
                    };
                    break (cause, (MarkMoved::default(), 0));
                }
                Some(LaneWake::Network(woke)) => {
                    work = RepositoryWork::Refresh;
                    log(&moved_line(woke));
                    break (Causes::NETWORK, (MarkMoved::default(), 0));
                }
                Some(LaneWake::Inputs(requested, classes, looks)) => {
                    // An unfinished request still owes its original stronger policy.
                    work = if ended == Ended::Ran {
                        requested
                    } else {
                        work.max(requested)
                    };
                    let cause = match requested {
                        RepositoryWork::Repair => Causes::INPUT_REPAIR,
                        RepositoryWork::Refresh => Causes::INPUT_REFRESH,
                    };
                    break (cause, (classes, looks));
                }
            }
        };
        note = StartNote {
            causes: cause,
            moved: moved.0,
            moves: moved.1,
        };
    }
}

/// The live lane's wait: `every`, or until `moved` hears this window's network move.
#[cfg(unix)]
fn until_moved(
    moved: &mut crate::network_moved::Watch,
    wake: &mut aterm_secrets::client::ConvergenceWake,
    inputs: &mut aterm_secrets::client::ConvergenceInputs,
    every: Option<std::time::Duration>,
) -> Option<LaneWake> {
    let deadline = every.and_then(|every| std::time::Instant::now().checked_add(every));
    loop {
        // Metadata-only name traffic cannot starve a due network move or postpone the
        // original retry/maintenance deadline while semantic snapshots produce no work.
        let now = std::time::Instant::now();
        if let Some(woke) = moved.take_at(now) {
            return Some(LaneWake::Network(woke));
        }
        if deadline.is_some_and(|at| now >= at) {
            return Some(LaneWake::Network(crate::network_moved::Woke::Timer));
        }
        let bell = match wake.arm() {
            Ok(bell) => bell,
            Err(generation) => {
                let snapshot = inputs.snapshot();
                let work = snapshot
                    .repositories
                    .map(|request| request.work())
                    .or_else(|| {
                        (snapshot.identity == aterm_secrets::client::IdentityDebt::Changed)
                            .then_some(aterm_secrets::converge::RepositoryWork::Repair)
                    });
                // Retain the semantic debt in this same synchronous lane before releasing
                // the event generation. Newer events during capture/child stay outstanding.
                wake.acknowledge(generation);
                if let Some(work) = work {
                    let (moved, looks) = snapshot.moved;
                    return Some(LaneWake::Inputs(work, moved, looks));
                }
                continue;
            }
        };
        match moved.wait_until(&[&bell], deadline) {
            Ok(Some(woke)) => return Some(LaneWake::Network(woke)),
            Ok(None) => continue, // input bell, not cancellation of the lane
            Err(_) => {
                return Some(LaneWake::Network(moved.wait(
                    deadline.map_or(every.unwrap_or(crate::secrets_join::MEND_EVERY), |at| {
                        at.saturating_duration_since(std::time::Instant::now())
                    }),
                )));
            }
        }
    }
}

/// The aterm.log line for a pass a move started: which source heard it.
#[cfg(unix)]
fn moved_line(woke: crate::network_moved::Woke) -> String {
    format!(
        "converge: the window's pass runs now: the network moved ({})",
        woke.words()
    )
}

/// Start one pass for the first-launch Secrets stage that just ended (`--trigger setup`),
/// without waiting for it: the setup's later stages go on. The process's reaper collects it
/// and queues the same completion observation as the window lane.
pub(crate) fn start_after_secrets() {
    #[cfg(unix)]
    if runs(false, false, &env_set) {
        after_secrets(&mut |trigger| {
            std::env::current_exe()
                .and_then(|exe| {
                    aterm_secrets::converge::start(
                        &exe,
                        trigger,
                        aterm_secrets::converge::RepositoryWork::Refresh,
                    )
                })
                .map(|child| hand_off_pass(child, pass_ended))
        });
    }
}

/// [`start_after_secrets`]'s start: `start` the Secrets stage's pass
/// ([`aterm_secrets::converge::SETUP`]), saying in aterm.log why it did not start.
#[cfg(unix)]
fn after_secrets(start: &mut dyn FnMut(&str) -> std::io::Result<()>) {
    if let Err(error) = start(aterm_secrets::converge::SETUP) {
        aterm_log::warn!("converge: the Secrets stage's pass did not start: {error}");
    }
}

/// How one pass of the lane ended.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Ended {
    /// It ran and settled (its own aterm.log line says what it did).
    Ran,
    /// It ran without failure but admitted work is waiting; retry quietly.
    Unfinished,
    /// It has standing work, owed again only once an input moves.
    Standing,
    /// It exited with this status (a failed row, a refusal): the runner's log line says why.
    Failed(Option<i32>),
    /// It could not be started or waited for: why.
    Unstarted(String),
}

#[cfg(unix)]
impl Ended {
    fn last(&self) -> aterm_secrets::converge::Last {
        use aterm_secrets::converge::Last;
        match self {
            Self::Ran => Last::Ended,
            Self::Unfinished => Last::Unfinished,
            Self::Standing => Last::Standing,
            Self::Failed(_) | Self::Unstarted(_) => Last::Failed,
        }
    }
}

/// The live lane's pass ([`LanePass`], which [`spawn_lane`] hands [`lane`]): [`pass`] from
/// `exe` under the trigger, the work and the note the lane gives it.
#[cfg(unix)]
fn window_pass(
    exe: &std::path::Path,
) -> impl FnMut(
    &str,
    aterm_secrets::converge::RepositoryWork,
    &aterm_secrets::converge::StartNote,
) -> Ended
+ '_ {
    move |trigger, work, note| pass(exe, trigger, work, note)
}

/// Run one pass for `trigger` from `exe`, started under `note`
/// ([`aterm_secrets::converge::noting`]), and wait for it, its wait for the lock included: how
/// it ended.
#[cfg(unix)]
fn pass(
    exe: &std::path::Path,
    trigger: &str,
    work: aterm_secrets::converge::RepositoryWork,
    note: &aterm_secrets::converge::StartNote,
) -> Ended {
    pass_starting(
        || {
            aterm_secrets::converge::noting(note, || {
                aterm_secrets::converge::start(exe, trigger, work)
            })
        },
        pass_ended,
    )
}

/// How the pass `start` starts ended, `ended` told once it ended.
#[cfg(unix)]
fn pass_starting(
    start: impl FnOnce() -> std::io::Result<aterm_uds::spawnfd::Child>,
    ended: impl FnOnce(),
) -> Ended {
    let mut child = match start() {
        Ok(child) => child,
        Err(error) => return Ended::Unstarted(error.to_string()),
    };
    match child.wait() {
        Ok(status) => {
            ended();
            ended_status(status)
        }
        Err(error) => Ended::Unstarted(error.to_string()),
    }
}

#[cfg(unix)]
fn ended_status(status: std::process::ExitStatus) -> Ended {
    match status.code() {
        _ if status.success() => Ended::Ran,
        Some(code) if code == i32::from(aterm_secrets::converge::UNFINISHED) => Ended::Unfinished,
        Some(code) if code == i32::from(aterm_secrets::converge::STANDING) => Ended::Standing,
        code => Ended::Failed(code),
    }
}

#[cfg(unix)]
fn hand_off_pass(mut child: aterm_uds::spawnfd::Child, mut ended: impl FnMut() + Send + 'static) {
    aterm_uds::reaper::hand_off(move || match child.try_wait() {
        Ok(Some(_)) => {
            ended();
            true
        }
        // spawnfd permanently retires custody after ECHILD. No terminal status was learned,
        // so release this waiter without turning lost custody into a completion notification.
        Err(error) if error.raw_os_error() == Some(libc::ECHILD) => true,
        Ok(None) | Err(_) => false,
    });
}

/// The aterm.log line for a lane pass that ended as `ended`; none for one that ran.
fn ended_line(ended: &Ended) -> Option<String> {
    match ended {
        Ended::Ran | Ended::Unfinished | Ended::Standing => None,
        Ended::Failed(code) => Some(format!(
            "converge: the window's pass ended {} (its own aterm.log line says which row \
             failed, or why the pass was refused)",
            code.map_or_else(
                || "by a signal".to_owned(),
                |code| format!("with exit {code}")
            )
        )),
        Ended::Unstarted(why) => Some(format!("converge: the window's pass did not run: {why}")),
    }
}

#[cfg(test)]
#[path = "converge_lane_tests.rs"]
mod tests;
