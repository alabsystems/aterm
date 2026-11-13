// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Durable observations of one process's lifetime, never execution authority.

use aterm_uds::process::{self, Birth, Identity, ProcessPresence};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::native_act::BootStamp;

/// One process's captured birth in its machine, boot and pid namespace. An exec keeps the
/// lifetime: Darwin's idversion is deliberately excluded. Restoring the data does not prove
/// that its process still exists; only a fresh [`Self::observe`] can say so.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessLifetime {
    scope: BootStamp,
    identity: Identity,
    namespace: Option<String>,
}

/// What a fresh kernel observation established about a captured process.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcessLiveness {
    /// Its exact birth still lives in the same kernel scope.
    Alive,
    /// Its process ended: positive absence/exit, a different birth, or a later boot here.
    Ended,
    /// The scope or a required kernel fact could not be read, or the observer is elsewhere.
    Unknown,
}

mod wire {
    use serde::{Deserialize, Serialize};

    #[derive(Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub(super) struct ProcessLifetime {
        pub(super) host: String,
        pub(super) boot: String,
        pub(super) pid: u32,
        pub(super) birth: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub(super) namespace: Option<String>,
    }
}

use wire::ProcessLifetime as Wire;

impl ProcessLifetime {
    /// Capture a live process around one kernel observation. Missing machine, boot, namespace
    /// or birth evidence supplies no lifetime. In particular this never upgrades a saved pid
    /// or whole-second start hint into evidence about its original process.
    #[must_use]
    pub fn capture(pid: u32) -> Option<Self> {
        let pid = i32::try_from(pid).ok().filter(|pid| *pid > 1)?;
        let scope = BootStamp::current()?;
        let namespace = process::pid_namespace();
        let birth = process::read_birth(pid)?;
        let captured = Self {
            scope,
            identity: Identity {
                pid,
                birth,
                idversion: None,
            },
            namespace,
        };
        (captured.canonical() && captured.observe() == ProcessLiveness::Alive).then_some(captured)
    }

    /// The process number this evidence names; it grants no right to signal that number.
    #[must_use]
    pub fn pid(&self) -> u32 {
        self.identity.pid.unsigned_abs()
    }

    pub(crate) fn retained_if_current(self) -> Option<Self> {
        (Self::capture(self.pid()).as_ref() == Some(&self)).then_some(self)
    }

    /// Observe the captured lifetime. Unknown reads remain unknown, never proof of exit.
    #[must_use]
    pub fn observe(&self) -> ProcessLiveness {
        let scope = BootStamp::current();
        let namespace = process::pid_namespace();
        let presence = process::process_presence(self.identity.pid);
        let birth = process::read_birth(self.identity.pid);
        let again = BootStamp::current();
        let namespace_again = process::pid_namespace();
        if scope != again || namespace != namespace_again {
            return ProcessLiveness::Unknown;
        }
        self.observed(scope.as_ref(), namespace.as_deref(), presence, birth)
    }

    fn observed(
        &self,
        scope: Option<&BootStamp>,
        namespace: Option<&str>,
        presence: ProcessPresence,
        birth: Option<Birth>,
    ) -> ProcessLiveness {
        use ProcessLiveness::{Alive, Ended, Unknown};
        let Some(scope) = scope.filter(|scope| scope.host == self.scope.host) else {
            return Unknown;
        };
        if scope.boot != self.scope.boot {
            return Ended;
        }
        if namespace != self.namespace.as_deref() {
            return Unknown;
        }
        if matches!(presence, ProcessPresence::Absent | ProcessPresence::Exited)
            || birth.is_some_and(|birth| birth != self.identity.birth)
        {
            return Ended;
        }
        if presence == ProcessPresence::Present && birth == Some(self.identity.birth) {
            Alive
        } else {
            Unknown
        }
    }

    fn canonical(&self) -> bool {
        let linux =
            self.scope.host.starts_with("linux:") || self.scope.host.starts_with("android:");
        self.scope.canonical()
            && self.identity.pid > 1
            && self.identity.birth.0 > 0
            && self.identity.idversion.is_none()
            && match self.namespace.as_deref() {
                Some(namespace) if linux => namespace
                    .parse::<u64>()
                    .is_ok_and(|number| number > 0 && number.to_string() == namespace),
                None => !linux,
                Some(_) => false,
            }
    }
}

impl Serialize for ProcessLifetime {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        Wire {
            host: self.scope.host.clone(),
            boot: self.scope.boot.clone(),
            pid: self.pid(),
            birth: self.identity.birth.0,
            namespace: self.namespace.clone(),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ProcessLifetime {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = Wire::deserialize(deserializer)?;
        let pid = i32::try_from(wire.pid).map_err(serde::de::Error::custom)?;
        let decoded = Self {
            scope: BootStamp {
                host: wire.host,
                boot: wire.boot,
            },
            identity: Identity {
                pid,
                birth: Birth(wire.birth),
                idversion: None,
            },
            namespace: wire.namespace,
        };
        decoded
            .canonical()
            .then_some(decoded)
            .ok_or_else(|| serde::de::Error::custom("noncanonical process lifetime"))
    }
}

#[cfg(test)]
#[path = "process_lifetime_tests.rs"]
mod tests;
