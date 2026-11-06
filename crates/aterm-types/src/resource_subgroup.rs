// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Private member requests within one already admitted job. This wire grants no new-root authority.

use crate::resource_admission::{self as admission, Binding, Cancel, Submission};
use std::io;

/// Bounded inherited lifetime fences; the receiver owns their interpretation and custody.
pub const MAX_FENCES: usize = 127;
/// Stdio/event four, shared CPU one, GNU two, and the bounded original fences.
pub const MAX_RIGHTS: usize = 4 + 1 + 2 + MAX_FENCES;
/// Single byte accompanying the exact ordered descriptor set.
pub const FD_MARKER: u8 = 0x46;
/// Binary handshake response after the job-scoped endpoint has authorized a request.
pub const READY: &[u8] = &[0x41];
/// Descriptor attachment acknowledgement, never evidence of launch.
pub const ATTACHED: &[u8] = &[1];
/// Existing runtime role negotiation with an untrusted original-job locator.
/// This selects an existing context only; the binary request must match it and
/// the host must still authenticate native peer, capability and live claim.
/// # Errors
/// Invalid canonical Binding representation.
pub fn hello(job: &Binding) -> io::Result<Vec<u8>> {
    Cancel {
        binding: job.clone(),
    }
    .encode()?;
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let attempt = job
        .attempt
        .iter()
        .flat_map(|byte| {
            [
                char::from(HEX[usize::from(*byte >> 4)]),
                char::from(HEX[usize::from(*byte & 15)]),
            ]
        })
        .collect::<String>();
    // Binding validation limits session to s- plus lowercase hex, so neither
    // string can contain JSON escapes or inject additional routing fields.
    Ok(format!(
        "{{\"version\":3,\"role\":\"member\",\"session\":\"{}\",\"attempt\":\"{attempt}\"}}",
        job.session
    )
    .into_bytes())
}
/// Runtime accepted the role and is ready for the bounded subgroup request.
pub const PREPARE: &[u8] = &[0x50];

fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "invalid resource subgroup frame",
    )
}

/// Job-private capability minted by its custodian before original root GO. Never log it.
#[derive(Clone, PartialEq, Eq)]
pub struct Capability([u8; 32]);
impl Capability {
    /// Retain the custodian's exact private random bytes; this constructor grants no authority.
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
    /// Borrow bytes for the host's capability check, never for diagnostics.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// Private routing context inherited from the actual custodian. Possession
/// alone proves no reservation: the host must check native peer membership,
/// live claim binding and the exact original job before accepting a request.
/// Intentionally has no diagnostic or serialization implementation.
pub struct JobContext {
    /// Original admitted job; this is never a new-root request.
    pub job: Binding,
    /// Original custodian-minted capability, never a public inspection field.
    pub capability: Capability,
}
impl JobContext {
    /// Encode only for explicit private child environment delivery.
    /// # Errors
    /// Invalid job binding or an empty capability.
    pub fn encode(&self) -> io::Result<String> {
        Cancel {
            binding: self.job.clone(),
        }
        .encode()?;
        if self.capability.0 == [0; 32] {
            return Err(invalid());
        }
        Ok(format!(
            "v3:{}:{}:{}",
            self.job.session,
            aterm_codec::hex::encode_lower(&self.job.attempt),
            aterm_codec::hex::encode_lower(&self.capability.0)
        ))
    }
    /// Decode untrusted environment bytes; no resource authority is created.
    /// # Errors
    /// Noncanonical, extra, missing or malformed fields.
    pub fn decode(value: &str) -> io::Result<Self> {
        if value.len() > 165 {
            return Err(invalid());
        }
        let fields: Vec<_> = value.split(':').collect();
        if fields.len() != 4 || fields[0] != "v3" {
            return Err(invalid());
        }
        let mut attempt = [0; 16];
        let mut capability = [0; 32];
        if !aterm_codec::hex::is_lower(fields[2], 32)
            || !aterm_codec::hex::is_lower(fields[3], 64)
            || !aterm_codec::hex::decode_into(fields[2].as_bytes(), &mut attempt)
            || !aterm_codec::hex::decode_into(fields[3].as_bytes(), &mut capability)
        {
            return Err(invalid());
        }
        let result = Self {
            job: Binding {
                session: fields[1].to_owned(),
                attempt,
            },
            capability: Capability(capability),
        };
        if result.encode()? != value {
            return Err(invalid());
        }
        Ok(result)
    }
}

/// CPU ownership is either a fixed subbudget or an actual inherited shared lease descriptor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CpuDelegation {
    /// Fixed CPU units borrowed within the original job, without a shared lease descriptor.
    FixedBorrow {
        /// Positive declared member CPU units.
        units: u32,
    },
    /// Borrow the transferred original shared CPU lease open description.
    SharedBorrow {
        /// Descriptor target inherited by the member.
        target_fd: u32,
    },
}
/// Original FIFO identity. Held descriptors prevent inode reuse while this witness is meaningful.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FifoWitness {
    /// Native filesystem device of the original FIFO.
    pub device: u64,
    /// Native inode, kept alive by the original transferred descriptor(s).
    pub inode: u64,
}
/// Preserve named GNU transport as named; numeric targets describe only actual numeric pipes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GnuTransport {
    /// No inherited GNU jobserver.
    None,
    /// Named authentication remains named, with original FIFO identity and held descriptors.
    NamedFifo {
        /// Lossless absolute GNU FIFO locator; never reopened by this transport.
        locator: Vec<u8>,
        /// Original FIFO identity checked by the host.
        witness: FifoWitness,
        /// One read/write descriptor or two ordered read/write descriptions.
        descriptors: u8,
    },
    /// Actual numeric pipe transport, retaining explicit child targets.
    NumericPipe {
        /// Child target of the original read description.
        read_target: u32,
        /// Child target of the original write description.
        write_target: u32,
    },
}
impl GnuTransport {
    /// Number of original GNU descriptors, after request validation.
    pub fn fd_count(&self) -> usize {
        match self {
            Self::None => 0,
            Self::NamedFifo { descriptors, .. } => usize::from(*descriptors),
            Self::NumericPipe { .. } => 2,
        }
    }
}

/// Private command and exact original-job/member bindings. Intentionally has no Debug.
#[derive(PartialEq, Eq)]
pub struct Request {
    /// Original admitted root job; never an instruction to admit a new root.
    pub job: Binding,
    /// Job-private capability minted before original root GO.
    pub capability: Capability,
    /// Existing lossless v3 command, peer, positive subbudget, label and host-owned timeout.
    pub member: Submission,
    /// Explicit fixed or shared CPU borrowing mode.
    pub cpu: CpuDelegation,
    /// Exact GNU transport representation.
    pub gnu: GnuTransport,
    /// Child descriptor targets, in the same order as the transferred original fences.
    pub fence_targets: Vec<u32>,
}

fn target(fd: u32) -> io::Result<()> {
    if (3..=i32::MAX as u32).contains(&fd) {
        Ok(())
    } else {
        Err(invalid())
    }
}
impl Request {
    /// Validate representation only; live claim, capability, peer and budget authority belong to the host.
    /// # Errors
    /// Invalid bindings, bounded command, descriptor layout or transport representation.
    pub fn validate(&self) -> io::Result<()> {
        self.member.validate()?;
        Cancel {
            binding: self.job.clone(),
        }
        .encode()?;
        if self.job.session != self.member.binding.session
            || self.job.attempt == self.member.binding.attempt
            || self.capability.0 == [0; 32]
            || self.fence_targets.len() > MAX_FENCES
        {
            return Err(invalid());
        }
        let mut targets = Vec::new();
        match self.cpu {
            CpuDelegation::FixedBorrow { units } if units > 0 && units == self.member.cpu_units => {
            }
            CpuDelegation::FixedBorrow { .. } => return Err(invalid()),
            CpuDelegation::SharedBorrow { target_fd } => targets.push(target_fd),
        }
        match &self.gnu {
            GnuTransport::None => {}
            GnuTransport::NamedFifo {
                locator,
                witness,
                descriptors,
            } => {
                if locator.first() != Some(&b'/')
                    || locator.len() > admission::MAX_FIELD
                    || locator.contains(&0)
                    || witness.inode == 0
                    || ![1, 2].contains(descriptors)
                {
                    return Err(invalid());
                }
            }
            GnuTransport::NumericPipe {
                read_target,
                write_target,
            } => {
                targets.push(*read_target);
                targets.push(*write_target);
            }
        }
        targets.extend(&self.fence_targets);
        for (i, fd) in targets.iter().enumerate() {
            target(*fd)?;
            if targets[..i].contains(fd) {
                return Err(invalid());
            }
        }
        Ok(())
    }
    /// Exact rights order: stdin, stdout, stderr, events, optional CPU, GNU descriptors, fences.
    /// # Errors
    /// Invalid request representation.
    pub fn fd_count(&self) -> io::Result<usize> {
        self.validate()?;
        Ok(
            4 + usize::from(matches!(self.cpu, CpuDelegation::SharedBorrow { .. }))
                + self.gnu.fd_count()
                + self.fence_targets.len(),
        )
    }
    /// Bounded subgroup envelope around existing v3 binding and Submission codecs.
    /// # Errors
    /// Invalid fields or total payload beyond the shared v3 bound.
    pub fn encode(&self) -> io::Result<Vec<u8>> {
        self.validate()?;
        let mut e = Encoder(vec![admission::VERSION, 4]);
        e.bytes(
            &Cancel {
                binding: self.job.clone(),
            }
            .encode()?,
        )?;
        e.0.extend(self.capability.0);
        e.bytes(&self.member.encode()?)?;
        match self.cpu {
            CpuDelegation::FixedBorrow { units } => {
                e.byte(0);
                e.u32(units);
            }
            CpuDelegation::SharedBorrow { target_fd } => {
                e.byte(1);
                e.u32(target_fd);
            }
        }
        match &self.gnu {
            GnuTransport::None => e.byte(0),
            GnuTransport::NamedFifo {
                locator,
                witness,
                descriptors,
            } => {
                e.byte(1);
                e.bytes(locator)?;
                e.u64(witness.device);
                e.u64(witness.inode);
                e.byte(*descriptors);
            }
            GnuTransport::NumericPipe {
                read_target,
                write_target,
            } => {
                e.byte(2);
                e.u32(*read_target);
                e.u32(*write_target);
            }
        }
        e.byte(self.fence_targets.len() as u8);
        for target in &self.fence_targets {
            e.u32(*target);
        }
        if e.0.len() > admission::MAX_PAYLOAD {
            return Err(invalid());
        }
        Ok(e.0)
    }
    /// Decode only for the original job already bound to the private listener.
    /// # Errors
    /// Wrong job, version, truncation, trailing bytes, oversized payload or invalid layout.
    pub fn decode(bytes: &[u8], expected_job: &Binding) -> io::Result<Self> {
        if bytes.len() > admission::MAX_PAYLOAD {
            return Err(invalid());
        }
        let mut d = Decoder(bytes);
        if d.take(2)? != [admission::VERSION, 4] {
            return Err(invalid());
        }
        let job = Cancel::decode(d.bytes()?, expected_job)?.binding;
        let capability = Capability(d.take(32)?.try_into().map_err(|_| invalid())?);
        let member = Submission::decode(d.bytes()?)?;
        let cpu = match d.byte()? {
            0 => CpuDelegation::FixedBorrow { units: d.u32()? },
            1 => CpuDelegation::SharedBorrow {
                target_fd: d.u32()?,
            },
            _ => return Err(invalid()),
        };
        let gnu = match d.byte()? {
            0 => GnuTransport::None,
            1 => GnuTransport::NamedFifo {
                locator: d.bytes()?.to_vec(),
                witness: FifoWitness {
                    device: d.u64()?,
                    inode: d.u64()?,
                },
                descriptors: d.byte()?,
            },
            2 => GnuTransport::NumericPipe {
                read_target: d.u32()?,
                write_target: d.u32()?,
            },
            _ => return Err(invalid()),
        };
        let count = usize::from(d.byte()?);
        if count > MAX_FENCES {
            return Err(invalid());
        }
        let mut fence_targets = Vec::with_capacity(count);
        for _ in 0..count {
            fence_targets.push(d.u32()?);
        }
        if !d.0.is_empty() {
            return Err(invalid());
        }
        let request = Self {
            job,
            capability,
            member,
            cpu,
            gnu,
            fence_targets,
        };
        request.validate()?;
        Ok(request)
    }
}
struct Encoder(Vec<u8>);
impl Encoder {
    fn byte(&mut self, n: u8) {
        self.0.push(n);
    }
    fn u32(&mut self, n: u32) {
        self.0.extend(n.to_be_bytes());
    }
    fn u64(&mut self, n: u64) {
        self.0.extend(n.to_be_bytes());
    }
    fn bytes(&mut self, v: &[u8]) -> io::Result<()> {
        if v.len() > admission::MAX_PAYLOAD
            || self.0.len().saturating_add(v.len()).saturating_add(4) > admission::MAX_PAYLOAD
        {
            return Err(invalid());
        }
        self.u32(v.len() as u32);
        self.0.extend(v);
        Ok(())
    }
}
struct Decoder<'a>(&'a [u8]);
impl<'a> Decoder<'a> {
    fn take(&mut self, n: usize) -> io::Result<&'a [u8]> {
        let bytes = self.0.get(..n).ok_or_else(invalid)?;
        self.0 = &self.0[n..];
        Ok(bytes)
    }
    fn byte(&mut self) -> io::Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn u32(&mut self) -> io::Result<u32> {
        Ok(u32::from_be_bytes(
            self.take(4)?.try_into().map_err(|_| invalid())?,
        ))
    }
    fn u64(&mut self) -> io::Result<u64> {
        Ok(u64::from_be_bytes(
            self.take(8)?.try_into().map_err(|_| invalid())?,
        ))
    }
    fn bytes(&mut self) -> io::Result<&'a [u8]> {
        let n = self.u32()? as usize;
        self.take(n)
    }
}

#[cfg(test)]
#[path = "resource_subgroup/tests.rs"]
mod tests;
