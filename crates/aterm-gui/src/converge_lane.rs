// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The converge pass's window lane (docs/DESIGN-init-on-by-default-2026-10-25.md §8.5): a
//! windowed instance starts `aterm init --converge --trigger window` off the event loop at once
//! and every [`crate::secrets_join::MEND_EVERY`] after, or at once when this window's network
//! moved ([`crate::network_moved`], docs/DESIGN-fluid-swarm-2026-10-28.md F3c), beside the vault
//! subscriber's mend. It covers a Mac whose subscriber runs a held copy of an older aterm, which
//! an update never restarts onto new code, and it is the trigger that joins an open vault (Local
//! Network is aterm.app's). The first-launch Secrets stage starts one pass of its own as it ends
//! ([`start_after_secrets`]).

#![cfg_attr(not(unix), allow(dead_code))]

/// Whether this process runs the converge pass's window lane, or the Secrets stage's pass: a
/// windowed instance (not `headless`) on a Unix desktop whose first-run setup is not running now
/// (`unified_setup`: the setup's own startup then starts the lane, `crate::start_converge_lane`,
/// and its Secrets stage a pass as it ends), outside a private state root and CI
/// (`ATERM_STATE_HOME`, `CI`, as the Commons runtime), `set` saying which of those variables are
/// set.
pub(crate) fn runs(headless: bool, unified_setup: bool, set: &dyn Fn(&str) -> bool) -> bool {
    cfg!(unix) && !headless && !unified_setup && !set("ATERM_STATE_HOME") && !set("CI")
}

/// Whether the environment variable `name` is set.
pub(crate) fn env_set(name: &str) -> bool {
    std::env::var_os(name).is_some()
}

/// Start the lane: a pass at once, then one every [`crate::secrets_join::MEND_EVERY`] or once
/// the network moved, each waited for on the lane's own thread (never the event loop) and its
/// exit said once per change. Whether the thread started; never joined.
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
                lane(
                    &mut |trigger| pass(&exe, trigger),
                    &mut |line| aterm_log::info!("{line}"),
                    &mut |every| until_moved(&mut moved, every),
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

/// The lane's rounds: a window pass at once ([`aterm_secrets::converge::WINDOW`]), then, each
/// time `wait` waited out [`crate::secrets_join::MEND_EVERY`] or was woken by a move, another;
/// `log` gets a pass's [`ended_line`] once per change of how passes end, and the source of each
/// move that started one ([`moved_line`]). It returns once `wait` says to stop (`None`), which
/// the live lane's never does.
#[cfg(unix)]
fn lane(
    pass: &mut dyn FnMut(&str) -> Ended,
    log: &mut dyn FnMut(&str),
    wait: &mut dyn FnMut(std::time::Duration) -> Option<crate::network_moved::Woke>,
) {
    let mut said = None;
    loop {
        let ended = pass(aterm_secrets::converge::WINDOW);
        if said.as_ref() != Some(&ended) {
            if let Some(line) = ended_line(&ended) {
                log(&line);
            }
            said = Some(ended);
        }
        match wait(crate::secrets_join::MEND_EVERY) {
            None => return,
            Some(crate::network_moved::Woke::Timer) => {}
            Some(woke) => log(&moved_line(woke)),
        }
    }
}

/// The live lane's wait: `every`, or until `moved` hears this window's network move.
#[cfg(unix)]
fn until_moved(
    moved: &mut crate::network_moved::Watch,
    every: std::time::Duration,
) -> Option<crate::network_moved::Woke> {
    Some(moved.wait(every))
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
/// without waiting for it: the setup's later stages go on. The process's reaper collects it.
pub(crate) fn start_after_secrets() {
    #[cfg(unix)]
    if runs(false, false, &env_set) {
        after_secrets(&mut |trigger| {
            std::env::current_exe()
                .and_then(|exe| aterm_secrets::converge::start(&exe, trigger))
                .map(drop)
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
    /// It ran (its own aterm.log line says what it did), or another pass held the lock.
    Ran,
    /// It exited with this status (a failed row, a refusal): the runner's log line says why.
    Failed(Option<i32>),
    /// It could not be started or waited for: why.
    Unstarted(String),
}

/// Run one pass for `trigger` from `exe` and wait for it.
#[cfg(unix)]
fn pass(exe: &std::path::Path, trigger: &str) -> Ended {
    let mut child = match aterm_secrets::converge::start(exe, trigger) {
        Ok(child) => child,
        Err(error) => return Ended::Unstarted(error.to_string()),
    };
    match child.wait() {
        Ok(status) if status.success() || status.code() == Some(aterm_secrets::converge::HELD) => {
            Ended::Ran
        }
        Ok(status) => Ended::Failed(status.code()),
        Err(error) => Ended::Unstarted(error.to_string()),
    }
}

/// The aterm.log line for a lane pass that ended as `ended`; none for one that ran.
fn ended_line(ended: &Ended) -> Option<String> {
    match ended {
        Ended::Ran => None,
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
mod tests {
    use super::*;

    /// The window's lane runs for a windowed launch with no first-run setup running, outside a
    /// private state root and CI; headless, a setup running now, `ATERM_STATE_HOME` or `CI` each
    /// skip it.
    /// NEGATIVE CONTROL: a gate that ignores `unified_setup` starts the lane beside a setup.
    #[test]
    fn the_window_lane_is_skipped_headless_under_setup_and_in_a_private_root_or_ci() {
        let none = |_: &str| false;
        assert_eq!(runs(false, false, &none), cfg!(unix), "an ordinary window");
        assert!(!runs(true, false, &none), "headless");
        assert!(!runs(false, true, &none), "under unified_setup");
        assert!(
            !runs(false, false, &|name| name == "ATERM_STATE_HOME"),
            "a private state root"
        );
        assert!(!runs(false, false, &|name| name == "CI"), "CI");
        assert!(
            runs(false, false, &|name| name == "HOME") || !cfg!(unix),
            "another variable does not skip it"
        );
    }

    /// The lane runs a window pass at once, then one each time it waited out `MEND_EVERY`, and
    /// logs how passes end once per change: a pass failing the same way again says nothing more.
    /// NEGATIVE CONTROL: a lane that waits before its first pass, or that logs every failed
    /// pass, fails here.
    #[cfg(unix)]
    #[test]
    fn the_lane_runs_a_window_pass_at_once_and_one_every_mend_interval() {
        let events = std::cell::RefCell::new(Vec::new());
        let mut ends = [
            Ended::Ran,
            Ended::Failed(Some(1)),
            Ended::Failed(Some(1)),
            Ended::Ran,
        ]
        .into_iter();
        let mut logged = Vec::new();
        lane(
            &mut |trigger| {
                events.borrow_mut().push(format!("pass {trigger}"));
                ends.next().expect("a scripted pass")
            },
            &mut |line| logged.push(line.to_owned()),
            &mut |every| {
                assert_eq!(every, crate::secrets_join::MEND_EVERY);
                let mut events = events.borrow_mut();
                events.push("wait".to_owned());
                (events.iter().filter(|event| *event == "wait").count() < 4)
                    .then_some(crate::network_moved::Woke::Timer)
            },
        );
        assert_eq!(
            *events.borrow(),
            [
                "pass window",
                "wait",
                "pass window",
                "wait",
                "pass window",
                "wait",
                "pass window",
                "wait"
            ]
        );
        assert_eq!(logged.len(), 1, "{logged:?}");
        assert!(logged[0].contains("exit 1"), "{logged:?}");
    }

    /// A route change this window's route source hears ends the lane's wait at once: a second
    /// pass runs, and aterm.log says the network moved and which source heard it.
    /// NEGATIVE CONTROL: unhook the lane's wait from the signal (`until_moved` that only sleeps),
    /// or the source from the route socket's stand-in: the wait runs to its hang detector, the
    /// wake reason reads `Timer`, and no line names the route change.
    #[cfg(unix)]
    #[test]
    fn the_converge_lane_wakes_on_a_route_change_and_says_why() {
        use crate::network_moved::{Moved, Woke, spawn_source, tests::HANG};
        let moved = std::sync::Arc::new(Moved::new());
        let mut watch = moved.watch();
        let (route, routes) = std::sync::mpsc::channel::<()>();
        assert!(spawn_source(
            "test-routes",
            std::sync::Arc::clone(&moved),
            (Woke::Route, std::time::Duration::ZERO),
            move || routes.recv().ok().map(|()| true)
        ));
        let passes = std::cell::Cell::new(0);
        let woke = std::cell::Cell::new(None);
        let mut logged = Vec::new();
        lane(
            &mut |trigger| {
                assert_eq!(trigger, aterm_secrets::converge::WINDOW);
                passes.set(passes.get() + 1);
                if passes.get() == 1 {
                    route.send(()).expect("the route source runs");
                }
                Ended::Ran
            },
            &mut |line| logged.push(line.to_owned()),
            &mut |every| {
                if passes.get() > 1 {
                    return None;
                }
                assert_eq!(every, crate::secrets_join::MEND_EVERY);
                let reason = until_moved(&mut watch, HANG.min(every));
                woke.set(reason);
                reason
            },
        );
        assert_eq!(woke.get(), Some(Woke::Route), "{logged:?}");
        assert_eq!(passes.get(), 2);
        assert_eq!(
            logged,
            ["converge: the window's pass runs now: the network moved (a route change)"]
        );
    }

    /// The Secrets stage's pass names the `setup` trigger, and one that does not start is no
    /// panic.
    /// NEGATIVE CONTROL: a start that names the window's trigger fails here.
    #[cfg(unix)]
    #[test]
    fn the_secrets_stage_starts_a_setup_pass() {
        let mut asked = Vec::new();
        after_secrets(&mut |trigger| {
            asked.push(trigger.to_owned());
            Err(std::io::Error::other("no such file"))
        });
        assert_eq!(asked, [aterm_secrets::converge::SETUP]);
    }

    /// A pass that ran, or met another pass's lock, says nothing (its runner logs its own line);
    /// one that failed or did not start says so.
    #[test]
    fn only_a_pass_that_did_not_run_right_is_logged_by_the_lane() {
        assert_eq!(ended_line(&Ended::Ran), None);
        assert!(ended_line(&Ended::Failed(Some(1))).is_some_and(|line| line.contains("exit 1")));
        assert!(
            ended_line(&Ended::Unstarted("no such file".to_owned()))
                .is_some_and(|line| line.contains("no such file"))
        );
    }
}
