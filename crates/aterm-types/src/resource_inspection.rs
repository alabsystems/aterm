// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Public read-only custody observations. JSON, paths and numbers never grant
//! admission or settlement. Only the authenticated host may report its actual
//! checked retained resources for the calling process's original launch.

use aterm_json::{Value, json};
use std::io;

/// Native root identity units; never mix them with Trust's Mach-clock owners.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BirthKind {
    /// proc_pidinfo wall start seconds * 1,000,000 + microseconds.
    MacosWallMicroseconds,
    /// /proc/PID/stat field 22 in native kernel clock ticks.
    LinuxStartTicks,
}
impl BirthKind {
    /// Stable public wire spelling.
    pub const fn wire(self) -> &'static str {
        match self {
            Self::MacosWallMicroseconds => "macos_wall_microseconds",
            Self::LinuxStartTicks => "linux_start_ticks",
        }
    }
}
/// Physical identity of the sealed native pool manifest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Manifest {
    /// Filesystem device.
    pub device: u64,
    /// Original manifest inode.
    pub inode: u64,
}
/// CPU custody is explicit: a cooperative ceiling is no fixed campaign grant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cpu {
    /// Actual complete retained fixed-width bundle held for this launch.
    RetainedFixed {
        /// Actual retained units.
        units: u32,
        /// Physical native pool identity shared by the debt domain.
        native_manifest: Manifest,
    },
    /// Native workers must acquire their own live shared credits.
    CooperativeShared {
        /// Borrowed root ceiling; each worker still needs a live credit.
        ceiling: u32,
        /// Physical native pool identity shared by the debt domain.
        native_manifest: Manifest,
    },
}
/// Actual owned launch scope that covers the querying native peer. Hosted
/// members are siblings of the original root, so ancestry must be explicit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Subject {
    /// The original admitted root or its independently checked descendants.
    Root {
        /// Actual original root PID.
        pid: u32,
        /// Original birth in the enclosing birth-kind units.
        birth: u64,
    },
    /// One durably bound host-owned subgroup, borrowing the root's memory.
    RegisteredMember {
        /// Actual member leader PID.
        pid: u32,
        /// Actual member birth in the enclosing birth-kind units.
        birth: u64,
        /// Nonreused member attempt, distinct from the root's attempt.
        attempt: [u8; 16],
    },
}
/// An exact durable memory claim observation, matching CohortClosureRef JSON.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Memory {
    /// The actual retained declared budget, in bytes.
    pub bytes: u64,
    /// Original namespace nonce.
    pub namespace: String,
    /// Original cover marker, not a process-control token.
    pub marker: String,
    /// Nonreused original claim epoch.
    pub epoch: String,
    /// Original guard device.
    pub guard_device: u64,
    /// Original guard inode.
    pub guard_inode: u64,
}
/// One authenticated host observation of actual bound original custody.
/// Callers must compare root_pid/root_birth with their own native identity;
/// arbitrary JSON with this shape is not evidence of admission.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Inspection {
    /// Exact canonical session identity.
    pub session: String,
    /// Original nonreused submission attempt.
    pub attempt: [u8; 16],
    /// Captured OS host identity.
    pub host: String,
    /// Kernel boot identity.
    pub boot: String,
    /// Original bound launched root.
    pub root_pid: u32,
    /// Actual native birth, in root_birth_kind units.
    pub root_birth: u64,
    /// Explicit platform birth units.
    pub root_birth_kind: BirthKind,
    /// Exact owned scope used to authenticate the querying peer.
    pub subject: Subject,
    /// Checked fixed bundle or cooperative native ceiling.
    pub cpu: Cpu,
    /// Actual original memory claim observation.
    pub memory: Memory,
}
fn hex(text: &str, size: usize) -> bool {
    aterm_codec::hex::is_lower(text, size)
}

fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "invalid resource custody observation",
    )
}
impl Inspection {
    /// Stable version-3 JSON. Validation only checks the observation's shape;
    /// the owner must independently validate live custody before constructing it.
    pub fn json(&self) -> io::Result<Value> {
        let sid = self.session.strip_prefix("s-").ok_or_else(invalid)?;
        let marker: Vec<_> = self.memory.marker.split('-').collect();
        if sid.is_empty()
            || sid.len() > 62
            || !hex(sid, sid.len())
            || self.host.is_empty()
            || self.host.len() > 256
            || self.boot.is_empty()
            || self.boot.len() > 128
            || !hex(&self.boot, self.boot.len())
            || self.root_pid == 0
            || self.root_birth == 0
            || self.memory.bytes == 0
            || self.memory.guard_inode == 0
            || !hex(&self.memory.namespace, 32)
            || !hex(&self.memory.epoch, 32)
            || marker.len() != 3
            || marker.iter().any(|part| {
                part.is_empty()
                    || !part.bytes().all(|b| b.is_ascii_digit())
                    || part.parse::<u64>().is_err()
            })
        {
            return Err(invalid());
        }
        let (width, manifest) = match self.cpu {
            Cpu::RetainedFixed {
                units,
                native_manifest,
            } => (units, native_manifest),
            Cpu::CooperativeShared {
                ceiling,
                native_manifest,
            } => (ceiling, native_manifest),
        };
        if width == 0 || manifest.inode == 0 {
            return Err(invalid());
        }
        let subject = match self.subject {
            Subject::Root { pid, birth } if pid == self.root_pid && birth == self.root_birth => {
                json!({"mode":"root","pid":pid,"birth":birth})
            }
            Subject::RegisteredMember {
                pid,
                birth,
                attempt,
            } if pid > 1 && pid != self.root_pid && birth > 0 && attempt != self.attempt => {
                json!({"mode":"registered_member","pid":pid,"birth":birth,"attempt":aterm_codec::hex::encode_lower(&attempt)})
            }
            _ => return Err(invalid()),
        };
        let native_manifest = json!({"device":manifest.device,"inode":manifest.inode});
        let cpu = match self.cpu {
            Cpu::RetainedFixed { units, .. } => {
                json!({"mode":"retained_fixed","units":units,"native_manifest":native_manifest})
            }
            Cpu::CooperativeShared { ceiling, .. } => {
                json!({"mode":"cooperative_shared","ceiling":ceiling,"native_manifest":native_manifest})
            }
        };
        Ok(
            json!({"version":3,"ready":true,"session":self.session,"attempt":aterm_codec::hex::encode_lower(&self.attempt),"host":self.host,"boot":self.boot,
            "root_pid":self.root_pid,"root_birth":self.root_birth,"root_birth_kind":self.root_birth_kind.wire(),"subject":subject,"cpu":cpu,
            "memory":{"bytes":self.memory.bytes,"claim":{"v":3,"namespace":self.memory.namespace,"marker":self.memory.marker,"epoch":self.memory.epoch,
                "guard":{"device":self.memory.guard_device,"inode":self.memory.guard_inode}}}}),
        )
    }
}
/// No grant, launch or timing outcome is implied by an unavailable inspector.
pub fn unavailable(reason: &str) -> Value {
    json!({"version":3,"ready":false,"reason":reason})
}

#[cfg(test)]
#[path = "resource_inspection_tests.rs"]
mod tests;
