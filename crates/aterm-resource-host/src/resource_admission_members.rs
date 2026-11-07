// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Live-job authorization precedes receiving delegated execution rights.
//! Routing bytes and an inherited capability never authorize a new root.
use super::*;
use aterm_types::resource_subgroup::{Capability, CpuDelegation, Request};

pub(in super::super) fn binding(value: &aterm_json::Value) -> io::Result<Binding> {
    binding_role(value, "member")
}
pub(in super::super) fn binding_role(value: &aterm_json::Value, role: &str) -> io::Result<Binding> {
    let map = value
        .as_object()
        .ok_or_else(|| refused("invalid member routing context"))?;
    let fields = ["version", "role", "session", "attempt"];
    if map.len() != fields.len()
        || fields.iter().any(|field| !map.contains_key(*field))
        || value["version"].as_u64() != Some(3)
        || value["role"].as_str() != Some(role)
    {
        return Err(refused("invalid member routing context"));
    }
    let session = value["session"]
        .as_str()
        .ok_or_else(|| refused("missing member session"))?;
    let encoded = value["attempt"]
        .as_str()
        .filter(|text| aterm_codec::hex::is_lower(text, 32))
        .ok_or_else(|| refused("invalid member attempt"))?;
    let mut attempt = [0; 16];
    if !aterm_codec::hex::decode_into(encoded.as_bytes(), &mut attempt) {
        return Err(refused("invalid member attempt"));
    }
    let binding = Binding {
        session: session.to_owned(),
        attempt,
    };
    aterm_types::resource_admission::Cancel {
        binding: binding.clone(),
    }
    .encode()?;
    Ok(binding)
}

pub(in super::super) struct Authorization {
    pub job: Binding,
    pub member: Binding,
    pub capability: Capability,
    pub peer: Peer,
    pub cpu: CpuDelegation,
    pub units: u32,
    pub memory_bytes: u64,
}
impl Authorization {
    pub(in super::super) fn of(request: &Request, peer: Peer) -> io::Result<Self> {
        request.validate()?;
        if !request.fence_targets.is_empty()
            || matches!(request.cpu, CpuDelegation::SharedBorrow { .. })
                && request.member.cpu_units != 1
        {
            return Err(refused(
                "member does not use the qualified shared-v3 descriptor route",
            ));
        }
        if request.member.peer_pid != peer.pid || request.member.peer_birth != peer.birth {
            return Err(refused("member caller differs from native peer"));
        }
        Ok(Self {
            job: request.job.clone(),
            member: request.member.binding.clone(),
            capability: request.capability.clone(),
            peer,
            cpu: request.cpu.clone(),
            units: request.member.cpu_units,
            memory_bytes: request.member.memory_bytes,
        })
    }
}
fn same_capability(left: &Capability, right: &Capability) -> bool {
    left.as_bytes()
        .iter()
        .zip(right.as_bytes())
        .fold(0u8, |difference, (a, b)| difference | (a ^ b))
        == 0
}
pub(super) fn trusted_caller(job: &Job, peer: Peer) -> bool {
    let Ok(pid) = i32::try_from(peer.pid) else {
        return false;
    };
    let birth = aterm_uds::process::Birth(peer.birth);
    if aterm_uds::process::read_birth(pid) != Some(birth) {
        return false;
    }
    let Some(image) = aterm_uds::process::LoadedImage::read(pid) else {
        return false;
    };
    let Ok(loaded_path) = image.launch_path().canonicalize() else {
        return false;
    };
    let mut programs = vec![job.program.clone()];
    if job.native_targo
        && let Some(directory) = job.program.parent()
    {
        programs.push(directory.join("trustc"));
    }
    programs.iter().any(|program| {
        program.canonicalize().is_ok_and(|path| path == loaded_path)
            && image.matches_file(program) == Some(true)
    }) && aterm_uds::process::read_birth(pid) == Some(birth)
}
pub(super) fn covered(job: &mut Job, peer: Peer) -> bool {
    aterm_uds::process::stable_descendant(
        peer.pid,
        aterm_uds::process::Birth(peer.birth),
        job.child.child_mut().id(),
        aterm_uds::process::Birth(job.root_birth),
        Instant::now() + Duration::from_millis(100),
    ) || job.members.values_mut().any(|member| member.covers(peer))
}
impl Owner {
    pub(in super::super) fn authorize_member(&mut self, request: &Authorization) -> bool {
        if self.pending.len()
            + self.jobs.len()
            + self
                .jobs
                .values()
                .map(|job| job.members.len())
                .sum::<usize>()
            >= aterm_capacity::host_protocol::MAX_JOBS
            || self.faulted
            || self.fence.check_current().is_err()
            || !self.journal.fresh(&request.member).unwrap_or(false)
        {
            return false;
        }
        let Some(job) = self
            .jobs
            .get_mut(&(request.job.session.clone(), request.job.attempt))
        else {
            return false;
        };
        live(&self.fence, &self.debt, job, request)
    }
}

pub(super) fn live(
    fence: &Fence,
    debt: &ExternalDebt,
    job: &mut Job,
    request: &Authorization,
) -> bool {
    if request.member.session != job.binding.session
        || request.member.attempt == job.binding.attempt
        || !same_capability(&request.capability, &job.delegation.capability)
        || !job.launched
        || job.reaped
        || job.settled
        || job.cleanup_at.is_some()
        || request.units == 0
        || request.units > job.cpu_ceiling
        || !trusted_caller(job, request.peer)
    {
        return false;
    }
    let mode_matches = matches!(
        (&job.custody, &request.cpu),
        (Custody::Cooperative(_), CpuDelegation::SharedBorrow { .. })
            | (Custody::Fixed(_), CpuDelegation::FixedBorrow { .. })
    );
    if !mode_matches {
        return false;
    }
    let Ok(coverage) = job::inspection(fence, debt, job) else {
        return false;
    };
    if request.memory_bytes > coverage.memory.bytes {
        return false;
    }
    covered(job, request.peer)
}
