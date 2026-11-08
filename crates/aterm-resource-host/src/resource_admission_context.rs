// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Native read-only context uses actual peer coverage and the original private
//! job capability. A codec observation creates no allocation or release right.
use super::*;
use aterm_types::resource_inspection as observed;
use trust_resource_controller as wire;

impl Owner {
    pub(in super::super) fn resolver_ready(
        &mut self,
        peer: Peer,
        query: &wire::ContextQuery,
    ) -> io::Result<wire::ContextReply> {
        let reply = self.context(peer, query)?;
        let job = self
            .jobs
            .get_mut(&(query.binding.session.clone(), query.binding.root_attempt))
            .ok_or_else(|| refused("resolver root is unavailable"))?;
        if job.metadata_root
            || !job
                .resolver
                .as_ref()
                .is_some_and(resolver_root::Preparation::prepared)
            || job.resolver_ready
            || job.cpu_ceiling != 1
            || job.metadata_gnu.is_none()
            || reply.context.sdk.is_none()
            || reply.context.original_root.pid != peer.pid
            || reply.context.original_root.birth != peer.birth
            || !crate::metadata_prelude::no_fork(peer.pid)?
        {
            return Err(refused(
                "resolver readiness requires its original confined unnegotiated root",
            ));
        }
        let sdk = job
            .metadata_sdk
            .as_ref()
            .ok_or_else(|| refused("resolver SDK missing"))?;
        // Preparation checked the marker on this same retained SDK before GO.
        // Fresh loaded/runtime validation below rejects any changed input;
        // repeating the marker parser cannot establish another fact here.
        sdk.validate_loaded(peer.pid)?;
        job.resolver_ready = true;
        Ok(reply)
    }

    pub(in super::super) fn metadata_ready(
        &mut self,
        peer: Peer,
        query: &wire::ContextQuery,
    ) -> io::Result<wire::ContextReply> {
        let reply = self.context(peer, query)?;
        let job = self
            .jobs
            .get_mut(&(query.binding.session.clone(), query.binding.root_attempt))
            .ok_or_else(|| refused("metadata root is unavailable"))?;
        if !job.metadata_root
            || job.metadata_ready
            || job.cpu_ceiling != 1
            || job.metadata_gnu.is_none()
            || reply.context.sdk.is_none()
            || reply.context.original_root.pid != peer.pid
            || reply.context.original_root.birth != peer.birth
        {
            return Err(refused(
                "metadata readiness requires its original live unnegotiated root",
            ));
        }
        job.metadata_sdk
            .as_ref()
            .ok_or_else(|| refused("metadata root SDK missing"))?
            .validate_loaded(peer.pid)?;
        // Readiness creates no new grant. It enables only the closed metadata
        // member protocol inside this already registered original root.
        job.metadata_ready = true;
        Ok(reply)
    }
    pub(in super::super) fn context(
        &mut self,
        peer: Peer,
        query: &wire::ContextQuery,
    ) -> io::Result<wire::ContextReply> {
        self.context_at(peer, query, Instant::now())
    }
    pub(super) fn context_at(
        &mut self,
        peer: Peer,
        query: &wire::ContextQuery,
        started: Instant,
    ) -> io::Result<wire::ContextReply> {
        query.encode()?;
        let key = (query.binding.session.clone(), query.binding.root_attempt);
        let job = self
            .jobs
            .get(&key)
            .ok_or_else(|| refused("unknown native job"))?;
        let difference = query
            .binding
            .capability
            .iter()
            .zip(job.delegation.capability.as_bytes())
            .fold(0u8, |sum, (left, right)| sum | (left ^ right));
        if difference != 0 || !job.launched || !members::trusted_caller(job, peer) {
            return Err(refused("native context capability or image refused"));
        }
        self.fence.check_current()?;
        if self.faulted {
            return Err(refused("native context custody is faulted"));
        }
        let job = self
            .jobs
            .get_mut(&key)
            .ok_or_else(|| refused("unknown native job"))?;
        if job.reaped || job.is_settled() || job.cleanup_at.is_some() {
            return Err(refused("native context scope is closing"));
        }
        let mut value = job::inspection(&self.fence, &self.debt, job)?;
        // Local native reads establish coverage; scheduler delay is no evidence
        // that the authenticated caller left its original scope.
        let deadline = started + Duration::from_secs(60);
        if !aterm_uds::process::stable_descendant(
            peer.pid,
            aterm_uds::process::Birth(peer.birth),
            job.child.child_mut().id(),
            aterm_uds::process::Birth(job.root_birth),
            deadline,
        ) {
            value.subject = job
                .members
                .values_mut()
                .find_map(|member| {
                    if Instant::now() < deadline && member.covers(peer) {
                        member.subject()
                    } else {
                        None
                    }
                })
                .ok_or_else(|| refused("native peer is outside this original job"))?;
        }
        let sdk = match value.subject {
            observed::Subject::Root { .. } => job.sdk.clone(),
            observed::Subject::RegisteredMember { attempt, .. } => {
                job.members.get(&attempt).and_then(member_queue::Slot::sdk)
            }
        };
        let mut context = convert(value)?;
        context.sdk = sdk;
        if peer.pid == context.original_root.pid
            && peer.birth == context.original_root.birth
            && (job.metadata_root || job.resolver.is_some())
        {
            context.sdk = job
                .metadata_sdk
                .as_ref()
                .map(|sdk| sdk.context())
                .transpose()?;
        }
        let reply = wire::ContextReply {
            call_attempt: query.call_attempt,
            context,
        };
        reply.encode()?;
        Ok(reply)
    }
}

fn nonce(text: &str) -> io::Result<[u8; 16]> {
    let mut out = [0; 16];
    if !aterm_codec::hex::is_lower(text, 32)
        || !aterm_codec::hex::decode_into(text.as_bytes(), &mut out)
    {
        return Err(refused("invalid original memory identity"));
    }
    Ok(out)
}
fn convert(value: Inspection) -> io::Result<wire::Context> {
    value.json()?;
    let birth_kind = match value.root_birth_kind {
        observed::BirthKind::MacosWallMicroseconds => wire::BirthKind::MacosWallMicroseconds,
        observed::BirthKind::LinuxStartTicks => wire::BirthKind::LinuxStartTicks,
    };
    let identity = |pid, birth| wire::Identity {
        pid,
        birth,
        birth_kind,
    };
    let manifest = |value: observed::Manifest| wire::FileIdentity {
        device: value.device,
        inode: value.inode,
    };
    Ok(wire::Context {
        session: value.session,
        root_attempt: value.attempt,
        host: value.host,
        boot: value.boot,
        original_root: identity(value.root_pid, value.root_birth),
        subject: match value.subject {
            observed::Subject::Root { pid, birth } => wire::Subject::Root {
                identity: identity(pid, birth),
            },
            observed::Subject::RegisteredMember {
                pid,
                birth,
                attempt,
            } => wire::Subject::RegisteredMember {
                identity: identity(pid, birth),
                attempt,
            },
        },
        cpu: match value.cpu {
            observed::Cpu::CooperativeShared {
                ceiling,
                native_manifest,
            } => wire::Cpu::CooperativeShared {
                ceiling,
                native_manifest: manifest(native_manifest),
            },
            observed::Cpu::RetainedFixed {
                units,
                native_manifest,
            } => wire::Cpu::RetainedFixed {
                units,
                native_manifest: manifest(native_manifest),
            },
        },
        memory: wire::Memory {
            bytes: value.memory.bytes,
            namespace: nonce(&value.memory.namespace)?,
            marker: value.memory.marker,
            epoch: nonce(&value.memory.epoch)?,
            guard: wire::FileIdentity {
                device: value.memory.guard_device,
                inode: value.memory.guard_inode,
            },
        },
        sdk: None,
    })
}
