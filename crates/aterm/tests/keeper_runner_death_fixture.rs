// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Ownership and progress waits for the two runner-death controls, in one fresh private world.

use std::path::Path;
use std::process::ExitStatus;

use aterm_uds::process::{ProcessPresence, read_lineage};
use aterm_uds::tether::Watched;

use crate::keeper_world::{GAP, HANG, World};

pub(super) struct Keeper(pub std::process::Child);

impl Drop for Keeper {
    fn drop(&mut self) {
        if let Err(error) = self.0.kill() {
            eprintln!("keeper fixture: direct child kill unavailable: {error}");
        }
        let reaped = aterm_spec::wait::within_while_working(
            self.0.id(),
            HANG,
            aterm_spec::wait::WORKING_GUARD,
            GAP,
            || self.0.try_wait().ok().flatten(),
        );
        if let Err(why) = reaped {
            eprintln!("keeper fixture: direct child reap unavailable: {why:?}");
        }
    }
}

pub(super) fn cancelled(root: &Path) -> bool {
    root.join("runner.cancel").exists()
}

/// Creation names the actual child; only the later serving line proves the keeper ran.
pub(super) fn publish_created(root: &Path, child: &std::process::Child) -> bool {
    let runner = std::process::id();
    let lineage =
        aterm_spec::wait::until_while_working(child.id(), "the new keeper's identity", || {
            if cancelled(root) {
                return Some(None);
            }
            let row = read_lineage(i32::try_from(child.id()).ok()?)?;
            (row.parent == runner).then_some(Some(row))
        });
    let Some(lineage) = lineage else {
        return false;
    };
    let watched = Watched::of(&lineage);
    let text = format!(
        "runner={runner} keeper={} uniqueid={}\n",
        watched.token().token(),
        watched.uniqueid
    );
    aterm_uds::atomic_file::write(&root.join("keeper.created"), text.as_bytes(), 0o600)
        .expect("publish this actor's actual keeper creation");
    true
}

fn created(root: &Path, runner: u32) -> Option<Watched> {
    let text = std::fs::read_to_string(root.join("keeper.created")).ok()?;
    let mut words = text.split_ascii_whitespace();
    if words.next()? != format!("runner={runner}") {
        return None;
    }
    let identity =
        aterm_uds::process::Identity::parse_shaped(words.next()?.strip_prefix("keeper=")?, false)?;
    let uniqueid = words
        .next()?
        .strip_prefix("uniqueid=")?
        .parse::<u64>()
        .ok()?;
    if words.next().is_some() {
        return None;
    }
    let watched = Watched {
        pid: identity.pid,
        birth: identity.birth,
        uniqueid,
    };
    (text
        == format!(
            "runner={runner} keeper={} uniqueid={}\n",
            watched.token().token(),
            watched.uniqueid
        ))
    .then_some(watched)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum State {
    Live,
    Gone,
    Unknown,
}

pub(super) fn state(watched: Watched) -> State {
    match aterm_uds::process::process_presence(watched.pid) {
        ProcessPresence::Absent => State::Gone,
        ProcessPresence::Exited if aterm_uds::process::exit_unfinished(watched.pid) => {
            State::Unknown
        }
        ProcessPresence::Exited => State::Gone,
        ProcessPresence::Present => match read_lineage(watched.pid) {
            Some(now)
                if now.identity.birth == watched.birth && now.uniqueid == watched.uniqueid =>
            {
                State::Live
            }
            Some(_) => State::Gone,
            None => State::Unknown,
        },
        ProcessPresence::Unknown => State::Unknown,
    }
}

fn retire_keeper(watched: Watched) -> bool {
    if state(watched) == State::Gone {
        return true;
    }
    let ended = aterm_uds::tether::end_watched(
        watched,
        aterm_uds::process::END_GRACE,
        &mut aterm_uds::process::EndControlLive,
        |pid| {
            let row = read_lineage(pid)?;
            Some(aterm_uds::tether::Proof {
                uniqueid: row.uniqueid,
                ran: aterm_uds::process::thread_ran(pid)?,
            })
        },
    );
    if let Err(why) = ended
        && state(watched) != State::Gone
    {
        eprintln!("keeper fixture: retirement refused for its created keeper: {why:?}");
        return false;
    }
    // A sent signal is not an exit. Do not release the world until the original identity is gone.
    aterm_spec::wait::within_while_working(
        watched.pid as u32,
        HANG,
        aterm_spec::wait::WORKING_GUARD,
        GAP,
        || (state(watched) == State::Gone).then_some(()),
    )
    .is_ok()
}

pub(super) struct Runner {
    pub child: aterm_uds::spawnfd::Child,
    world: Option<World>,
    keeper: Option<Watched>,
    exit: Option<ExitStatus>,
}

impl Runner {
    pub(super) fn new(child: aterm_uds::spawnfd::Child, world: World) -> Self {
        Self {
            child,
            world: Some(world),
            keeper: None,
            exit: None,
        }
    }

    pub(super) fn root(&self) -> &Path {
        &self.world.as_ref().expect("the guarded world").0
    }

    pub(super) fn ready(&mut self) -> Watched {
        let watched = aterm_spec::wait::until_while_working(
            self.child.id(),
            "the keeper's serving proof before its runner is killed",
            || {
                let watched = created(self.root(), self.child.id())?;
                let pid = std::fs::read_to_string(self.root().join("keeper.pid"))
                    .ok()?
                    .parse::<i32>()
                    .ok()?;
                (pid == watched.pid).then_some(watched)
            },
        );
        self.keeper = Some(watched);
        watched
    }

    pub(super) fn kill_and_reap(&mut self) -> ExitStatus {
        self.child.kill().expect("kill this owned test runner");
        let status = aterm_spec::wait::until_while_working(
            self.child.id(),
            "reap this owned test runner",
            || self.child.try_wait().expect("poll this owned test runner"),
        );
        self.exit = Some(status);
        status
    }

    pub(super) fn retire_keeper(&mut self) -> bool {
        let keeper = self
            .keeper
            .or_else(|| created(self.root(), self.child.id()));
        self.keeper = keeper;
        keeper.is_some_and(retire_keeper)
    }

    fn retire_runner(&mut self) -> bool {
        if self.exit.is_some() {
            return true;
        }
        // Let an actor still warming or awaiting its keeper unwind through its Child guard.
        let _ = std::fs::write(self.root().join("runner.cancel"), b"stop\n");
        let ended = aterm_spec::wait::within_while_working(
            self.child.id(),
            HANG,
            aterm_spec::wait::WORKING_GUARD,
            GAP,
            || self.child.try_wait().ok().flatten(),
        );
        match ended {
            Ok(status) => {
                self.exit = Some(status);
                true
            }
            Err(_) => {
                if let Err(error) = self.child.kill() {
                    eprintln!("keeper fixture: runner kill unavailable: {error}");
                }
                let reaped = aterm_spec::wait::within_while_working(
                    self.child.id(),
                    HANG,
                    aterm_spec::wait::WORKING_GUARD,
                    GAP,
                    || self.child.try_wait().ok().flatten(),
                );
                match reaped {
                    Ok(status) => {
                        self.exit = Some(status);
                        true
                    }
                    Err(why) => {
                        eprintln!("keeper fixture: runner retirement unavailable: {why:?}");
                        false
                    }
                }
            }
        }
    }
}

impl Drop for Runner {
    fn drop(&mut self) {
        let runner_gone = self.retire_runner();
        self.keeper = self
            .keeper
            .or_else(|| created(self.root(), self.child.id()));
        // Without its creation receipt, even an actor's ordinary panic exit says nothing
        // about whether its Child guard could reap the keeper. Keep the world in that case.
        let keeper_gone = self.keeper.is_some_and(retire_keeper);
        if (!runner_gone || !keeper_gone || std::thread::panicking())
            && let Some(world) = self.world.take()
        {
            eprintln!(
                "KEPT {}: keeper-death fixture did not establish complete cleanup or its case failed",
                world.0.display()
            );
            std::mem::forget(world);
        }
    }
}
