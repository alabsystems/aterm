// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Shared domain types for `aterm-machines` probes, host facts, and agent reports.

use serde::{Deserialize, Serialize};
use std::fmt;

pub use aterm_converge::{Effect, ItemReport, Mode, Outcome, Probe, Read};

/// A preference value: the property-list types `defaults` stores.
///
/// A dictionary keeps its keys in document order; [`PValue::same`] compares dictionaries
/// without regard to it, because `defaults export` sorts keys and a baseline need not.
#[derive(Debug, Clone, PartialEq)]
pub enum PValue {
    /// `<true/>` / `<false/>`.
    Bool(bool),
    /// `<integer>`.
    Int(i64),
    /// `<real>`.
    Real(f64),
    /// `<string>`.
    Str(String),
    /// `<data>`, decoded.
    Data(Vec<u8>),
    /// `<date>`, as the ISO 8601 text the plist carries.
    Date(String),
    /// `<array>`.
    Array(Vec<PValue>),
    /// `<dict>`, in document order.
    Dict(Vec<(String, PValue)>),
}

impl PValue {
    /// Whether `self` satisfies `wanted` (comparing dicts as key sets and `0`/`1` as `bool`).
    #[must_use]
    pub fn same(&self, wanted: &Self) -> bool {
        match (self, wanted) {
            (Self::Int(i), Self::Bool(b)) | (Self::Bool(b), Self::Int(i)) => {
                (*i == 1 && *b) || (*i == 0 && !*b)
            }
            (Self::Int(i), Self::Real(r)) | (Self::Real(r), Self::Int(i)) => {
                // The half-open bounds are exact powers of two. Checking them before
                // conversion also excludes i64::MAX rounded upward to 2^63.
                const LIMIT: f64 = 9_223_372_036_854_775_808.0;
                if !r.is_finite() || r.fract() != 0.0 || !(-LIMIT..LIMIT).contains(r) {
                    false
                } else {
                    #[expect(
                        clippy::cast_possible_truncation,
                        reason = "finite integral value checked inside the exact i64 range"
                    )]
                    let integer = *r as i64;
                    *i == integer
                }
            }
            (Self::Array(a), Self::Array(b)) => {
                a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.same(y))
            }
            (Self::Dict(a), Self::Dict(b)) => {
                a.len() == b.len()
                    && b.iter()
                        .all(|(k, v)| a.iter().any(|(ka, va)| ka == k && va.same(v)))
            }
            (a, b) => a == b,
        }
    }
}

impl fmt::Display for PValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bool(b) => write!(f, "{b}"),
            Self::Int(i) => write!(f, "{i}"),
            Self::Real(r) => write!(f, "{r}"),
            Self::Str(s) => write!(f, "{s:?}"),
            Self::Data(d) => write!(f, "<{} bytes>", d.len()),
            Self::Date(d) => write!(f, "{d}"),
            Self::Array(items) => {
                f.write_str("[")?;
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{item}")?;
                }
                f.write_str("]")
            }
            Self::Dict(entries) => {
                f.write_str("{")?;
                for (i, (k, v)) in entries.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{k} = {v}")?;
                }
                f.write_str("}")
            }
        }
    }
}

/// One keyboard the host sees.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Keyboard {
    /// USB/Bluetooth vendor id (0 for Apple silicon's built-in keyboard).
    pub vendor: u32,
    /// Product id (0 for Apple silicon's built-in keyboard).
    pub product: u32,
    /// The product name `hidutil list` prints.
    pub name: String,
    /// Built into this Mac.
    pub builtin: bool,
}

/// A local account, as the agent resolved it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct User {
    /// Short name.
    pub name: String,
    /// uid.
    pub uid: u32,
    /// Primary gid.
    pub gid: u32,
    /// Home directory.
    pub home: String,
    /// Login shell.
    #[serde(default)]
    pub shell: String,
    /// Member of `admin`.
    pub admin: bool,
}

/// What kind of Mac this is, for items that exist on one kind only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum Form {
    /// Has a battery.
    Laptop,
    /// No battery.
    Desktop,
    /// A virtual machine (`VirtualMac*`).
    Virtual,
    /// Not determined.
    #[default]
    Unknown,
}

/// What the agent knows about the host it runs on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct HostFacts {
    /// `scutil --get ComputerName`.
    pub computer_name: String,
    /// `scutil --get LocalHostName` (the `.local` Bonjour name, without `.local`).
    pub local_host_name: String,
    /// IOPlatformUUID: the identity envelopes are bound to.
    pub uuid: String,
    /// IOPlatformSerialNumber.
    pub serial: String,
    /// `hw.model`, e.g. `Mac16,1`.
    pub model: String,
    /// Marketing name (`hardware::model_name`); empty when unknown.
    #[serde(default)]
    pub model_name: String,
    /// Chip or processor family (`sysctl -n machdep.cpu.brand_string`); empty when unknown.
    #[serde(default)]
    pub chip: String,
    /// An Intel processor's speed, `3.1 GHz`; empty on Apple silicon and when unknown.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub processor_speed: String,
    /// Memory in bytes (`sysctl -n hw.memsize`); 0 when unknown.
    #[serde(default)]
    pub memory_bytes: u64,
    /// Largest internal disk size in bytes (`hardware::internal_disk`), or `0` when unknown.
    #[serde(default)]
    pub disk_bytes: u64,
    /// That disk is a hard disk (`Medium Type` `Rotational`), named by its decimal size.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub disk_rotational: bool,
    /// `arm64` or `x86_64` (`uname -m`).
    pub arch: String,
    /// `sw_vers -productVersion`.
    pub os_version: String,
    /// `sw_vers -buildVersion`.
    pub os_build: String,
    /// Laptop, desktop or virtual machine.
    #[serde(default)]
    pub form: Form,
    /// FileVault on (`fdesetup isactive`); `None` when it could not be read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filevault: Option<bool>,
    /// Unix seconds of the last boot (`kern.boottime`).
    #[serde(default)]
    pub boot_time: u64,
    /// Seconds since the last keyboard or mouse input (`HIDIdleTime`); `None` unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_secs: Option<u64>,
    /// Whether SSH sessions hold Full Disk Access.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_full_disk_access: Option<bool>,
    /// Whether the item runner holds Full Disk Access (`kTCCServiceSystemPolicyAllFiles`),
    /// measured without a prompt ([`crate::port::System::full_disk_access`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub full_disk_access: Option<bool>,
    /// Owner of `/dev/console` (`None` at the login window; `(uid <n>)` or `(unknown)` if unnamed).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub console_user: Option<String>,
    /// Local accounts with a uid ≥ 501.
    pub users: Vec<User>,
    /// Keyboards `hidutil list` sees.
    pub keyboards: Vec<Keyboard>,
    /// SHA-256 (hex) of each artifact the agent's root cache holds.
    pub cached: Vec<String>,
    /// The agent's version: the running aterm's product version (`aterm_stamp::app_version`).
    pub agent_version: String,
}

/// Report returned by one agent run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostReport {
    /// [`crate::SCHEMA`].
    pub schema: u32,
    /// Plan or apply.
    pub mode: Mode,
    /// The envelope serial this run consumed (0 for a local `here` run).
    pub serial: u64,
    /// The signing key's id (`envelope::key_id`), empty for a local run.
    #[serde(default)]
    pub key_id: String,
    /// This run's id (`state/runs/<id>/` on the host).
    #[serde(default)]
    pub run_id: String,
    /// Unix seconds at start and at the end.
    pub started: u64,
    /// Unix seconds when the last item finished.
    pub finished: u64,
    /// The host.
    pub facts: HostFacts,
    /// One row per item, in run order.
    pub items: Vec<ItemReport>,
    /// Set when the agent refused the run as a whole (bad signature, wrong host, replay).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refused: Option<String>,
}

impl HostReport {
    /// Whether this run's exit status is nonzero.
    #[inline]
    #[must_use]
    pub fn failed(&self) -> bool {
        self.refused.is_some() || self.items.iter().any(|i| i.outcome.is_failure())
    }

    /// How many items ended with `outcome`.
    #[inline]
    #[must_use]
    pub fn count(&self, outcome: Outcome) -> usize {
        self.items.iter().filter(|i| i.outcome == outcome).count()
    }
}

#[cfg(test)]
#[path = "model_tests.rs"]
mod tests;

/// Integer/real equivalence is exact; finite ordinary integral values remain equivalent.
#[cfg(test)]
#[path = "model_integer_real_boundary_tests.rs"]
mod integer_real_boundary_tests;
