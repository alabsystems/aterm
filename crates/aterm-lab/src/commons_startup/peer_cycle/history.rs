// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Restore an absent peer from the ordinary observer's retained history.

use super::{
    Cycle, HANG, MachineIdentity, NetworkObservation, StageEvidence, fail, same_identity, save,
};
use crate::{Fail, instance::World, stop};
use aterm_json::json;
use aterm_types::network_state::{HistoryState, Reachability, Visibility};
use aterm_uds::process::{ProcessPresence, process_presence};
use std::path::Path;
use std::time::{Duration, Instant};

fn retained_matches(
    retained: &NetworkObservation,
    previous: &NetworkObservation,
    peer: &MachineIdentity,
) -> bool {
    if retained.history != HistoryState::Ready
        || retained.visibility != Visibility::Unavailable
        || retained.observer.is_some()
        || retained.via_local_process.is_some()
        || retained.revision < previous.revision
        || retained.observed_at_ms < previous.observed_at_ms
    {
        return false;
    }
    let selected = |network: &NetworkObservation| {
        network
            .peers
            .iter()
            .find(|candidate| same_identity(&candidate.identity, peer))
            .map(|candidate| {
                (
                    candidate.slot,
                    candidate.first_seen_ms,
                    candidate.last_seen_ms,
                    candidate.last_checked_ms,
                    candidate.reachability,
                )
            })
    };
    let (Some(current), Some(old)) = (selected(retained), selected(previous)) else {
        return false;
    };
    current.0 == old.0
        && current.1 == old.1
        && current.1 != 0
        && current.2 == old.2
        && current.2 != 0
        && current.2 <= retained.observed_at_ms
        && current.3.is_none()
        && current.4 == Reachability::Unknown
}

fn absent(pids: &[i32], out: &Path) -> Result<(), Fail> {
    let until = Instant::now() + HANG;
    loop {
        stop::check()?;
        let observations = pids
            .iter()
            .map(|pid| (*pid, process_presence(*pid)))
            .collect::<Vec<_>>();
        let passed = observations
            .iter()
            .all(|(_, presence)| *presence == ProcessPresence::Absent);
        if passed || Instant::now() >= until {
            save(
                out,
                "peer-cycle-observer-stopped.json",
                &json!({
                    "passed":passed,
                    "processes":observations.iter().map(|(pid,presence)|json!({
                        "pid":pid,"presence":format!("{presence:?}"),
                    })).collect::<Vec<_>>(),
                    "scope":"Kernel absence of the owned observer, its keeper and the stopped source before history read or replacement launch; no signal sent by this read",
                }),
            )?;
            return if passed {
                Ok(())
            } else {
                Err(Fail::die("owned observer, keeper or source is not absent"))
            };
        }
        stop::sleep(Duration::from_millis(100))?;
    }
}

impl Cycle<'_> {
    fn checkpoint(
        &mut self,
        pid: u32,
        peer: &MachineIdentity,
        lost: &StageEvidence,
    ) -> Result<(), Fail> {
        let config = self.recipient.cfg.join("aterm/aterm.toml");
        let until = Instant::now() + HANG;
        loop {
            stop::check()?;
            self.continuing_window(pid)?;
            let retained = aterm_db::network_state::read_history(&config);
            self.continuing_window(pid)?;
            let matches = retained
                .as_ref()
                .is_ok_and(|retained| retained_matches(retained, &lost.network, peer));
            if matches || retained.is_err() || Instant::now() >= until {
                save(
                    self.out,
                    "peer-cycle-history-checkpoint.json",
                    &json!({
                        "passed":matches,"observer_pid":pid,
                        "network":retained.as_ref().ok().map(aterm_db::network_state::value),
                        "error":retained.as_ref().err().map(ToString::to_string),
                        "scope":"Read-only checkpoint projection with exact lost-stage peer data before observer stop; a derived LastSeen alone does not establish a completed checkpoint",
                    }),
                )?;
                return if matches {
                    Ok(())
                } else {
                    Err(Fail::die(
                        "lost-stage peer data was not observed in the retained checkpoint",
                    ))
                };
            }
            stop::sleep(Duration::from_millis(100))?;
        }
    }

    pub(super) fn restart(
        mut self,
        pid: u32,
        source_pid: u32,
        peer: &MachineIdentity,
        lost: &StageEvidence,
    ) -> Result<Self, Fail> {
        self.continuing_window(pid)?;
        self.checkpoint(pid, peer, lost)?;
        let config = self.recipient.cfg.join("aterm/aterm.toml");
        let configuration = crate::util::read_regular_text(&config).map_err(fail)?;
        // A fresh launch directory keeps the first process's logs. Its data roots stay exact.
        let mut launch = World::new(&configuration).map_err(fail)?;
        super::super::keep(&mut launch);
        launch.home.clone_from(&self.recipient.home);
        launch.cfg.clone_from(&self.recipient.cfg);
        launch.run.clone_from(&self.recipient.run);
        let executable = self.window.aterm.clone();
        let keeper = self.window.pid;
        self.window.stop().map_err(fail)?;
        absent(&[pid as i32, keeper, source_pid as i32], self.out)?;
        // This is the read-only projection of a completed checkpoint. The replacement owner
        // will independently reopen the astream journal; no live producer is running here.
        let retained = aterm_db::network_state::read_history(&config).map_err(fail)?;
        let matches = retained_matches(&retained, &lost.network, peer);
        save(
            self.out,
            "peer-cycle-retained-history.json",
            &json!({
                "passed":matches,"source_pid":source_pid,"stopped_observer_pid":pid,
                "network":aterm_db::network_state::value(&retained),
                "scope":"Read-only local checkpoint projection with both hosts stopped; this cache is not live reachability or the replacement owner's journal read",
            }),
        )?;
        if !matches {
            return Err(Fail::die(
                "absent peer did not survive in retained checkpoint history",
            ));
        }
        // Drop the stopped Instance, including its quiet claim, before another window starts.
        drop(self.window);
        self.window = super::super::start_window(&launch, &executable, self.port, false)?;
        let restarted = self.window.aterm_pid();
        save(
            self.out,
            "peer-cycle-observer-restarted.json",
            &json!({
                "previous_observer_pid":pid,"observer_pid":restarted,
                "keeper_pid":self.window.pid,"launch_profile":launch.dir,
                "home":launch.home,"config":launch.cfg,"runtime":launch.run,
                "source_restarted":false,
            }),
        )?;
        if restarted.is_none_or(|restarted| restarted <= 0 || restarted as u32 == pid) {
            return Err(Fail::die(
                "replacement observer process was not established",
            ));
        }
        Ok(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restored_checkpoint_requires_original_peer_history_without_live_evidence() {
        let lost = super::super::tests::observation();
        let peer = &lost.peers[0].identity;
        let mut retained = lost.clone();
        retained.visibility = Visibility::Unavailable;
        retained.observer = None;
        retained.via_local_process = None;
        retained.peers[0].reachability = Reachability::Unknown;
        retained.peers[0].last_checked_ms = None;
        assert!(retained_matches(&retained, &lost, peer));
        for field in 0..12 {
            let mut wrong = retained.clone();
            match field {
                0 => wrong.history = HistoryState::Unavailable,
                1 => wrong.visibility = Visibility::Active,
                2 => wrong.observer = lost.observer.clone(),
                3 => wrong.via_local_process = lost.via_local_process,
                4 => wrong.revision -= 1,
                5 => wrong.observed_at_ms -= 1,
                6 => wrong.peers.clear(),
                7 => wrong.peers[0].identity.incarnation = [9; 16],
                8 => wrong.peers[0].slot += 1,
                9 => wrong.peers[0].first_seen_ms += 1,
                10 => wrong.peers[0].last_seen_ms += 1,
                _ => wrong.peers[0].last_checked_ms = Some(99_000),
            }
            assert!(!retained_matches(&wrong, &lost, peer), "field {field}");
        }
        retained.peers[0].reachability = Reachability::Reachable;
        assert!(!retained_matches(&retained, &lost, peer));
    }
}
