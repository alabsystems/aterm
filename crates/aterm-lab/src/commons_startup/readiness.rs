// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Read-only startup admission, before this scenario's first publication attempt.

use super::{HANG, Node, fail, save};
use crate::instance::{Instance, World};
use crate::{Fail, stop};
use aterm_json::{Value, json};
use aterm_secrets::{
    client::Paths,
    commons::{binding::Binding, bootstrap::Bootstrap, journal::Trust, roster::Role},
};
use aterm_types::network_state::{HistoryState, NetworkObservation, Reachability, Visibility};
use std::{
    path::Path,
    time::{Duration, Instant},
};

#[derive(Clone)]
struct Admission {
    current: bool,
    local: Binding,
    role: Role,
    log_backed: bool,
    head: [u8; 32],
    history_head: [u8; 32],
    basis: [u8; 32],
    generation: u64,
    members: Vec<(Binding, Role)>,
}

#[derive(Clone)]
struct Observation {
    trust: Trust,
    requested: Binding,
    admission: Option<Admission>,
    network: NetworkObservation,
    sampled_at: Instant,
}

fn read(paths: &Paths, until: Instant) -> Result<Observation, &'static str> {
    let sampled_at = Instant::now();
    let (bootstrap, journal) = Bootstrap::established_with_journal_until(paths, until)
        .map_err(|_| "established-custody-or-control-unavailable")?;
    let access = bootstrap
        .access_in(&journal)
        .map_err(|_| "membership-unavailable")?;
    let requested = bootstrap
        .request()
        .map_err(|_| "binding-unavailable")?
        .binding()
        .clone();
    let mut admission = access.as_ref().map(|access| Admission {
        current: false,
        local: access.local().binding().clone(),
        role: access.local().role(),
        log_backed: access.is_log_backed(),
        head: *access.head(),
        history_head: *access.control_history_head(),
        basis: *access.basis(),
        generation: access.generation(),
        members: access
            .members()
            .iter()
            .map(|member| (member.binding().clone(), member.role()))
            .collect(),
    });
    if let (Some(admission), Some(access)) = (&mut admission, &access) {
        admission.current = bootstrap.revalidate_custody().is_ok() && access.is_current(&journal);
    }
    // Only public metadata leaves this local read. Drop access, journal and bootstrap before
    // the other replica is read, relay IPC is attempted, or the caller waits or publishes.
    Ok(Observation {
        trust: bootstrap.trust(),
        requested,
        admission,
        network: NetworkObservation::default(),
        sampled_at,
    })
}

fn ready(
    observed: &[Observation; 2],
    expected: &[Binding; 2],
    pids: [u32; 2],
    log_backed: bool,
    now: Instant,
    wall_ms: Option<u64>,
) -> bool {
    let Some(wall) = wall_ms else {
        return false;
    };
    let recent = |stamp| stamp != 0 && wall.checked_sub(stamp).is_some_and(|age| age <= 15_000);
    if expected[0].machine() == expected[1].machine()
        || expected[0].actor() == expected[1].actor()
        || observed[0].trust != observed[1].trust
    {
        return false;
    }
    let (Some(left), Some(right)) = (&observed[0].admission, &observed[1].admission) else {
        return false;
    };
    if left.head != right.head
        || left.history_head != right.history_head
        || left.basis != right.basis
        || left.generation != right.generation
    {
        return false;
    }
    observed.iter().enumerate().all(|(index, observed)| {
        let admission = observed
            .admission
            .as_ref()
            .expect("both admissions established");
        let local = &expected[index];
        let peer = &expected[1 - index];
        let network = &observed.network;
        observed.requested == *local
            && admission.local == *local
            && *local.network() == observed.trust.network
            && *local.authority() == observed.trust.authority
            && admission.current
            && admission.role == Role::Writer
            && admission.log_backed == log_backed
            && admission.members.len() == 2
            && expected.iter().all(|binding| {
                admission
                    .members
                    .iter()
                    .any(|(member, role)| member == binding && *role == Role::Writer)
            })
            && now
                .checked_duration_since(observed.sampled_at)
                .is_some_and(|age| age <= Duration::from_secs(15))
            && network.visibility == Visibility::Active
            && network.history == HistoryState::Ready
            && network.via_local_process == Some(pids[index])
            && recent(network.observed_at_ms)
            && network.observer.as_ref().is_some_and(|identity| {
                identity.machine_id == *local.machine()
                    && identity.incarnation == *local.incarnation()
            })
            && network.peers.iter().any(|observed| {
                observed.identity.machine_id == *peer.machine()
                    && observed.identity.incarnation == *peer.incarnation()
                    && observed.reachability == Reachability::Reachable
                    && recent(observed.last_seen_ms)
                    && observed.last_seen_ms <= network.observed_at_ms
                    && observed
                        .last_checked_ms
                        .is_some_and(|at| recent(at) && at <= network.observed_at_ms)
            })
    })
}

fn binding_value(binding: &Binding) -> Value {
    let mut hash = aterm_digest::Sha256::new();
    hash.update(b"aterm-lab-readiness-binding-v1");
    hash.update(binding.network());
    hash.update(binding.authority());
    hash.update(binding.machine());
    hash.update(binding.recipient().0);
    hash.update(binding.writer());
    hash.update(binding.incarnation());
    json!({"machine":aterm_codec::hex::encode_lower(binding.machine()),
        "incarnation":aterm_codec::hex::encode_lower(binding.incarnation()),
        "binding_digest":aterm_codec::hex::encode_lower(&hash.finalize())})
}

fn value(observed: &Observation) -> Value {
    let mut hash = aterm_digest::Sha256::new();
    hash.update(observed.trust.network);
    hash.update(observed.trust.authority);
    hash.update(observed.trust.genesis);
    json!({"network_id":aterm_codec::hex::encode_lower(&observed.trust.network),
    "trust_digest":aterm_codec::hex::encode_lower(&hash.finalize()),
    "requested":binding_value(&observed.requested),
    "network":{
        "revision":observed.network.revision,"observed_at_ms":observed.network.observed_at_ms,
        "via_local_process":observed.network.via_local_process,
        "visibility":format!("{:?}",observed.network.visibility),"history":format!("{:?}",observed.network.history),
        "observer":observed.network.observer.as_ref().map(|identity|json!({
            "machine":aterm_codec::hex::encode_lower(&identity.machine_id),
            "incarnation":aterm_codec::hex::encode_lower(&identity.incarnation),
        })),
        "peers":observed.network.peers.iter().map(|peer|json!({
            "slot":peer.slot,"machine":aterm_codec::hex::encode_lower(&peer.identity.machine_id),
            "incarnation":aterm_codec::hex::encode_lower(&peer.identity.incarnation),
            "last_seen_ms":peer.last_seen_ms,"last_checked_ms":peer.last_checked_ms,
            "reachability":format!("{:?}",peer.reachability),
        })).collect::<Vec<_>>(),
    },
    "admission":observed.admission.as_ref().map(|admission| json!({
        "current":admission.current,"local":binding_value(&admission.local),
        "role":format!("{:?}",admission.role),"log_backed":admission.log_backed,
        "head":aterm_codec::hex::encode_lower(&admission.head),
        "control_history_head":aterm_codec::hex::encode_lower(&admission.history_head),
        "basis":aterm_codec::hex::encode_lower(&admission.basis),"generation":admission.generation,
        "members":admission.members.iter().map(|(binding,role)|json!({
            "binding":binding_value(binding),"role":format!("{role:?}"),
        })).collect::<Vec<_>>(),
    }))})
}

fn hosts_live(node: &mut Node, window: &mut Instance, pid: u32) -> Result<bool, Fail> {
    Ok(node.0.try_wait().map_err(fail)?.is_none()
        && window.running()
        && window.aterm_pid() == Some(pid as i32))
}

/// Observe convergence of the two expected writers and their authenticated contacts.
/// This creates no custody/catalog and grants no lease for the subsequent single write.
pub(super) fn wait(
    worlds: [&World; 2],
    node: &mut Node,
    window: &mut Instance,
    log_backed: bool,
    out: &Path,
) -> Result<(), Fail> {
    let pids = [
        node.0.id(),
        window
            .aterm_pid()
            .ok_or_else(|| Fail::die("readiness recipient has no app pid"))? as u32,
    ];
    let paths = worlds.map(|world| Paths::for_home(&world.home));
    let until = Instant::now() + HANG;
    let mut expected = None;
    let mut last = Value::Null;
    loop {
        stop::check()?;
        if !hosts_live(node, window, pids[1])? {
            save(
                out,
                "startup-readiness-failed.json",
                &json!({"passed":false,"reason":"owned-host-exited-or-changed","last":last}),
            )?;
            return Err(Fail::die(
                "owned host exited or changed before startup readiness",
            ));
        }
        let observed = (|| {
            let mut observed = [read(&paths[0], until)?, read(&paths[1], until)?];
            // Neither replica's custody/access/journal handles survive into these IPC reads.
            for (world, observed) in worlds.iter().zip(&mut observed) {
                observed.network = aterm_db::network_state::read_live_observation(
                    &world.cfg.join("aterm/aterm.toml"),
                )
                .map_err(|_| "live-observer-unavailable")?;
            }
            Ok::<_, &'static str>(observed)
        })();
        match observed {
            Ok(observed) => {
                let expected = expected.get_or_insert_with(|| {
                    observed.each_ref().map(|value| value.requested.clone())
                });
                last = json!({"replicas":observed.iter().map(value).collect::<Vec<_>>(),
                    "expected":expected.iter().map(binding_value).collect::<Vec<_>>()});
                if ready(
                    &observed,
                    expected,
                    pids,
                    log_backed,
                    Instant::now(),
                    aterm_time::unix_millis(),
                ) && hosts_live(node, window, pids[1])?
                {
                    save(
                        out,
                        "startup-readiness.json",
                        &json!({
                            "passed":true,"observation":last,"observer_pids":pids,
                            "scope":"Current read-only two-writer admission and authenticated contacts; not atomic authorization of the following publication",
                        }),
                    )?;
                    return Ok(());
                }
            }
            Err(reason) => last = json!({"read_boundary":reason}),
        }
        if Instant::now() >= until {
            save(
                out,
                "startup-readiness-failed.json",
                &json!({"passed":false,"reason":"readiness-not-established","last":last}),
            )?;
            return Err(Fail::die(
                "two-writer admission and authenticated contacts did not establish startup readiness",
            ));
        }
        stop::sleep(Duration::from_millis(100))?;
    }
}

#[cfg(test)]
#[path = "readiness_tests.rs"]
mod tests;
