// SPDX-License-Identifier: Apache-2.0 OR MIT
// Copyright 2026 Andrew Yates

/// Maximum private frame, including command and complete environment.
pub const MAX_PAYLOAD: usize = 1024 * 1024;
/// Maximum lossless argument, environment entry or pathname.
pub const MAX_FIELD: usize = 65536;
/// Maximum arguments or environment entries.
pub const MAX_ITEMS: usize = 4096;
/// Compatibility bound; current v3 production accepts no lifetime fences.
pub const MAX_FENCES: usize = 127;
/// Default private helper entry for fixed-FD fixtures. Production formats its
/// held socket numbers with HelperFds::arguments; neither carries a grant.
pub const HELPER_ARGUMENTS: [&str; 4] =
    ["__resource-member-v3", "--protocol=3", "--control-fd=3", "--producer-fd=4"];
/// The helper's private control endpoint; never inherited by the target.
pub const CONTROL_FD: u32 = 3;
/// The helper's producer-proof endpoint; never inherited by the target.
pub const PRODUCER_FD: u32 = 4;

/// Private original-root context. Intentionally has no diagnostic formatter.
#[derive(Clone, PartialEq, Eq)]
pub struct JobBinding {
    pub session: String,
    pub root_attempt: [u8; 16],
    pub capability: [u8; 32],
}

/// Command bytes are Unix OS bytes, never lossy UTF-8. Environment is a complete
/// snapshot; the helper must not merge in its own ambient environment.
#[derive(Clone, PartialEq, Eq)]
pub struct CommandSpec {
    pub program: Vec<u8>,
    pub arg0: Option<Vec<u8>>,
    pub args: Vec<Vec<u8>>,
    pub environment: Vec<(Vec<u8>, Vec<u8>)>,
    pub cwd: Vec<u8>,
}

/// Helper source and future target descriptors inhabit different namespaces.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Descriptor {
    pub source: u32,
    pub target: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GnuTransport {
    None,
    NumericPipe { read: Descriptor, write: Descriptor },
    NamedFifo { locator: Vec<u8>, device: u64, inode: u64, read: Descriptor, write: Descriptor },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Delegations {
    pub cpu: Option<Descriptor>,
    pub gnu: GnuTransport,
    pub fences: Vec<Descriptor>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    MetadataVersion,
    Compile,
}

/// One attempt. Unknown delivery must never cause this request to be replayed.
/// Intentionally has no Debug implementation because it contains a capability.
#[derive(Clone, PartialEq, Eq)]
pub struct Request {
    pub binding: JobBinding,
    pub member_attempt: [u8; 16],
    pub role: Role,
    pub command: CommandSpec,
    pub delegations: Delegations,
    pub timeout_ns: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BirthKind {
    MacosWallMicroseconds,
    LinuxStartTicks,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Identity {
    pub pid: u32,
    pub birth: u64,
    pub birth_kind: BirthKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Started {
    pub identity: Identity,
    pub monotonic_ns: u64,
    pub unix_ns: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Exit(i32),
    Signal(i32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompletionCause {
    Exited,
    Timeout,
    Cancelled,
    ResourceLimit,
}

/// Finished is delivered only after authenticated scope retirement and resource
/// return. Runtime measures target execution, excluding retirement cleanup.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    Attached,
    Started(Started),
    Finished { outcome: Outcome, cause: CompletionCause, runtime_ns: u64 },
    Refused { reason: String },
    Unknown { reason: String, started: Option<Started> },
}

/// Frames bind every fact/cancellation to the same original and member attempt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatusFrame {
    pub root_attempt: [u8; 16],
    pub member_attempt: [u8; 16],
    pub status: Status,
}
