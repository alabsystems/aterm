// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Starting the converge pass's runner (`aterm init --converge`, `aterm_machines::sync`), the
//! pass that keeps a Mac aligned with the lab's expected state after an update, a network move
//! or half an hour (docs/DESIGN-init-on-by-default-2026-10-25.md §8.5).
//!
//! Three places start it, each naming itself with `--trigger`: the vault's subscriber (a
//! launchd job: as it starts, which is the moment after an update replaced its program, after
//! each version it applies, and in a healthy round once [`Due`] says so), the aterm window's
//! lane, and the first-launch Secrets stage as it ends. Each starts [`runner`] in a session of
//! its own and never disclaims responsibility for it, so macOS keeps attributing the runner's
//! Local Network use to the app that started it.

use crate::client::SyncOutcome;
use aterm_uds::spawnfd::{Child, Command, Stdio};
use std::net::IpAddr;
use std::path::Path;

/// The exit status a runner ends with while another pass holds the pass's lock: it did not run.
pub const HELD: i32 = 75;

/// How long after it started a runner the subscriber starts another on the same routes: the
/// pass's own recheck (`aterm_machines::sync::RECHECK`).
pub const RECHECK_MS: u64 = 30 * 60 * 1000;

/// The runner for `trigger` (`subscriber`, `window` or `setup`): `exe init --converge --trigger
/// <trigger>`, reading and writing nothing of its starter's (stdio null), in a session of its
/// own (`setsid`), so a launchd job's restart, which ends its process group, or a terminal's
/// hangup does not end a pass midway. Responsibility is never disclaimed: the runner's Local
/// Network use stays the starting app's.
#[must_use]
pub fn runner(exe: &Path, trigger: &str) -> Command {
    let mut command = aterm_uds::spawnfd::command(exe);
    command
        .args(["init", "--converge", "--trigger", trigger])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .setsid(true);
    command
}

/// Start [`runner`] for `trigger` and return at once: the caller waits for it, or drops it to
/// the process's reaper, never blocking on the pass.
///
/// # Errors
/// The runner could not be started.
pub fn start(exe: &Path, trigger: &str) -> std::io::Result<Child> {
    runner(exe, trigger).spawn()
}

/// This Mac's routes as the subscriber compares them (`crate::server::route_sources`).
pub type Routes = [Option<IpAddr>; 3];

/// How the subscriber's last runner stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Last {
    /// It started none.
    None,
    /// It still runs.
    Running,
    /// It ended, having run a pass or failed.
    Ended,
    /// It ended without running, another pass holding the lock ([`HELD`]).
    Held,
}

impl Last {
    /// How `child`, the last runner started, stands now, reaping it once it ended and never
    /// waiting for it.
    pub fn of(child: Option<&mut Child>) -> Self {
        let Some(child) = child else {
            return Self::None;
        };
        match child.try_wait() {
            Ok(None) => Self::Running,
            Ok(Some(status)) if status.code() == Some(HELD) => Self::Held,
            Ok(Some(_)) | Err(_) => Self::Ended,
        }
    }
}

/// When the vault's subscriber starts a runner: as it starts, after each pass that applied a
/// version, and after any other healthy pass once this Mac's routes moved or [`RECHECK_MS`]
/// passed (or the clock went back) since the last start. One runner at a time: a start that
/// comes due while the last one runs, or after one that found the lock held, is owed to the
/// next round, so a version applied during a pass is never left to the next recheck.
#[derive(Debug, Default)]
pub struct Due {
    /// When the last runner started, in ms since the epoch, and the routes then.
    started: Option<(u64, Routes)>,
    /// A start came due while the last runner ran, or the last one found the lock held.
    owed: bool,
}

impl Due {
    /// Whether the subscriber starts a runner as it starts: always, unless one it started still
    /// runs (none can yet), at `now_ms` with `routes`.
    pub fn at_start(&mut self, now_ms: u64, routes: Routes, last: Last) -> bool {
        self.start(true, now_ms, routes, last)
    }

    /// Whether the subscriber starts a runner after a healthy pass that ended with `outcome`, at
    /// `now_ms` with `routes`, its last runner standing as `last`.
    pub fn after_pass(
        &mut self,
        outcome: &SyncOutcome,
        now_ms: u64,
        routes: Routes,
        last: Last,
    ) -> bool {
        let applied = matches!(outcome, SyncOutcome::Applied { .. });
        self.start(applied, now_ms, routes, last)
    }

    fn start(&mut self, called: bool, now_ms: u64, routes: Routes, last: Last) -> bool {
        if last == Last::Held {
            self.owed = true;
        }
        let due = called
            || self.owed
            || self
                .started
                .is_none_or(|(at, was)| was != routes || now_ms < at || now_ms - at >= RECHECK_MS);
        if !due {
            return false;
        }
        if last == Last::Running {
            self.owed = true;
            return false;
        }
        self.owed = false;
        self.started = Some((now_ms, routes));
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::materialize::Report;
    use std::net::Ipv4Addr;

    const HOME: Routes = [Some(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 7))), None, None];
    const AWAY: Routes = [Some(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 9))), None, None];
    const MINUTE: u64 = 60 * 1000;

    fn applied() -> SyncOutcome {
        SyncOutcome::Applied {
            seq: 3,
            report: Report::default(),
        }
    }

    fn unchanged() -> SyncOutcome {
        SyncOutcome::Unchanged { seq: 3 }
    }

    /// One start as the subscriber starts, one more for each version it applies, none for a pass
    /// that changed nothing on the same routes inside the recheck, one when the routes move and
    /// one once the recheck passes (or the clock goes back).
    /// NEGATIVE CONTROL: an `after_pass` that reads an applied version as a plain round starts
    /// nothing for it.
    #[test]
    fn the_subscriber_starts_a_runner_at_start_per_applied_version_and_when_due() {
        let mut due = Due::default();
        let starts = std::cell::Cell::new(0);
        let at = |started: bool| starts.set(starts.get() + usize::from(started));
        at(due.at_start(0, HOME, Last::None));
        assert_eq!(starts.get(), 1, "one at start");
        at(due.after_pass(&applied(), MINUTE, HOME, Last::Ended));
        assert_eq!(starts.get(), 2, "+1 for an applied version");
        at(due.after_pass(&applied(), 2 * MINUTE, HOME, Last::Ended));
        assert_eq!(starts.get(), 3, "+1 for each");
        for minute in 3..30 {
            at(due.after_pass(&unchanged(), minute * MINUTE, HOME, Last::Ended));
        }
        assert_eq!(starts.get(), 3, "0 for passes that changed nothing");
        at(due.after_pass(&unchanged(), 30 * MINUTE, AWAY, Last::Ended));
        assert_eq!(starts.get(), 4, "+1 when the routes move");
        at(due.after_pass(&unchanged(), 31 * MINUTE, AWAY, Last::Ended));
        assert_eq!(starts.get(), 4, "not again on the routes it started on");
        at(due.after_pass(&unchanged(), 60 * MINUTE, AWAY, Last::Ended));
        assert_eq!(starts.get(), 5, "+1 once the recheck passed");
        at(due.after_pass(&unchanged(), MINUTE, AWAY, Last::Ended));
        assert_eq!(starts.get(), 6, "+1 for a clock that went back");
    }

    /// A start that comes due while the last runner runs waits for it to end rather than start a
    /// second that would find the lock held; one that found the lock held starts again at the
    /// next round, as does nothing else.
    #[test]
    fn a_start_due_while_a_runner_runs_or_after_a_held_lock_is_owed_to_the_next_round() {
        let mut due = Due::default();
        assert!(due.at_start(0, HOME, Last::None));
        assert!(
            !due.after_pass(&applied(), MINUTE, HOME, Last::Running),
            "never two at once"
        );
        assert!(
            !due.after_pass(&unchanged(), 2 * MINUTE, HOME, Last::Running),
            "still running"
        );
        assert!(
            due.after_pass(&unchanged(), 3 * MINUTE, HOME, Last::Ended),
            "the owed start, once it ended"
        );
        assert!(!due.after_pass(&unchanged(), 4 * MINUTE, HOME, Last::Ended));
        assert!(
            due.after_pass(&unchanged(), 5 * MINUTE, HOME, Last::Held),
            "another pass held the lock: start again"
        );
        assert!(!due.after_pass(&unchanged(), 6 * MINUTE, HOME, Last::Ended));
    }

    /// The runner is `aterm init --converge --trigger <who>` of the program given, and never
    /// disclaims responsibility (macOS then keeps the runner's Local Network use the starting
    /// app's); its own session is proven on a started child below.
    /// NEGATIVE CONTROL: a runner that disclaims responsibility fails here.
    #[test]
    fn the_runner_is_init_converge_and_never_disclaims_responsibility() {
        let exe = Path::new("/Applications/aterm.app/Contents/MacOS/aterm");
        let command = runner(exe, "window");
        assert_eq!(command.get_program(), exe);
        let args: Vec<_> = command.get_args().collect();
        assert_eq!(args, ["init", "--converge", "--trigger", "window"]);
        assert!(
            !command.get_disclaim_responsibility(),
            "the runner's Local Network use stays the starting app's"
        );
    }

    /// A runner that does not end by itself leaves the start, and the reading of how it stands,
    /// returning at once: the subscriber's long-poll never waits on a pass. The child leads a
    /// process group of its own (the session `setsid` made), and one that ends with [`HELD`]
    /// reads as held.
    #[cfg(unix)]
    #[test]
    fn starting_a_runner_never_waits_for_it_and_gives_it_its_own_session() {
        use std::os::unix::fs::PermissionsExt as _;
        let scratch = aterm_tempfile::tempdir().expect("scratch");
        let sh = aterm_uds::deadline::stand_in(Path::new("/bin/sh"));
        let sleep = aterm_uds::deadline::stand_in(Path::new("/bin/sleep"));
        let script = |name: &str, body: String| {
            let path = scratch.path().join(name);
            std::fs::write(&path, format!("#!{}\n{body}\n", sh.display())).expect("script");
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))
                .expect("executable");
            path
        };
        let endless = script("endless", format!("exec {} 600", sleep.display()));
        let mut child = start(&endless, "subscriber").expect("start a runner");
        assert_eq!(Last::of(Some(&mut child)), Last::Running);
        let pid = i32::try_from(child.id()).expect("a pid");
        let lineage = aterm_uds::process::read_lineage(pid).expect("its lineage");
        assert_eq!(
            lineage.pgid,
            child.id(),
            "it leads a group (and the session setsid made) of its own"
        );
        child.kill().expect("kill the stand-in");
        child.wait().expect("reap it");
        assert_eq!(Last::of(Some(&mut child)), Last::Ended);
        let held = script("held", format!("exit {HELD}"));
        let mut child = start(&held, "subscriber").expect("start a runner");
        child.wait().expect("reap it");
        assert_eq!(Last::of(Some(&mut child)), Last::Held);
        assert_eq!(Last::of(None), Last::None);
    }
}
