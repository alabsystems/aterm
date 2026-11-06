// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Native read-only context uses actual peer coverage and the original private
//! job capability. A codec observation creates no allocation or release right.
use super::*;
use aterm_types::resource_inspection as observed;
use trust_resource_controller as wire;

impl Owner {
    pub(in super::super) fn context(
        &mut self,
        peer: Peer,
        query: &wire::ContextQuery,
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
        if job.reaped || job.settled || job.cleanup_at.is_some() {
            return Err(refused("native context scope is closing"));
        }
        let mut value = job::inspection(&self.fence, &self.debt, job)?;
        if !aterm_uds::process::stable_descendant(
            peer.pid,
            aterm_uds::process::Birth(peer.birth),
            job.child.child_mut().id(),
            aterm_uds::process::Birth(job.root_birth),
            Instant::now() + Duration::from_millis(100),
        ) {
            let deadline = Instant::now() + Duration::from_millis(100);
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
        let reply = wire::ContextReply {
            call_attempt: query.call_attempt,
            context: convert(value)?,
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
    })
}
