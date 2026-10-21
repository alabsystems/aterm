// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The vocabulary every half shares: what a probe read, what the plan is, what happened,
//! and the report one agent run returns. A read, a probe, an outcome and a report row are
//! the converge engine's (`aterm_converge`), re-exported here so the families name them
//! as before.

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
    /// Whether `self` (what the host holds) already satisfies `wanted`.
    ///
    /// Dictionaries compare as key sets. A boolean is satisfied by the integer macOS's
    /// own readers take for it (`boolForKey:` reads `1` as true): System Settings writes
    /// some switches as integers, and rewriting an equivalent value would report a change
    /// that changes nothing.
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
    /// The one-line form reports print: TOML-like scalars, `[…]` and `{k = v, …}`.
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
    /// The model's marketing name, `MacBook Pro`, `Mac mini`, `Virtual Machine`
    /// (`hardware::model_name`); empty when unknown.
    #[serde(default)]
    pub model_name: String,
    /// The chip, `Apple M4 Max`, or an Intel processor's family, `Intel Core i7`
    /// (`sysctl -n machdep.cpu.brand_string`); empty when unknown.
    #[serde(default)]
    pub chip: String,
    /// An Intel processor's speed, `3.1 GHz`; empty on Apple silicon and when unknown.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub processor_speed: String,
    /// Memory in bytes (`sysctl -n hw.memsize`); 0 when unknown.
    #[serde(default)]
    pub memory_bytes: u64,
    /// The Mac's internal disk in bytes, the whole disk as `ioreg` lists it (the largest
    /// internal one: `hardware::internal_disk`); 0 when unknown.
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
    /// Commands run over SSH have Full Disk Access (Remote Login's "Allow full disk
    /// access for remote users"): the system TCC database opened for reading.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_full_disk_access: Option<bool>,
    /// Who owns `/dev/console` (absent at the login window). An owner that could not be
    /// named reads `(uid <n>)` or `(unknown)` (`facts::CONSOLE_UNKNOWN`), never absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub console_user: Option<String>,
    /// Local accounts with a uid ≥ 501.
    pub users: Vec<User>,
    /// Keyboards `hidutil list` sees.
    pub keyboards: Vec<Keyboard>,
    /// SHA-256 (hex) of each artifact the agent's root cache holds.
    pub cached: Vec<String>,
    /// The agent's version (`CARGO_PKG_VERSION`).
    pub agent_version: String,
}

/// What one agent run returns (design §9).
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
    /// Whether this run's exit status is nonzero (design §9).
    #[must_use]
    pub fn failed(&self) -> bool {
        self.refused.is_some() || self.items.iter().any(|i| i.outcome.is_failure())
    }

    /// How many items ended with `outcome`.
    #[must_use]
    pub fn count(&self, outcome: Outcome) -> usize {
        self.items.iter().filter(|i| i.outcome == outcome).count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_boolean_is_satisfied_by_the_integer_macos_reads_for_it() {
        assert!(PValue::Int(1).same(&PValue::Bool(true)));
        assert!(PValue::Int(0).same(&PValue::Bool(false)));
        assert!(!PValue::Int(2).same(&PValue::Bool(true)));
        assert!(!PValue::Int(1).same(&PValue::Bool(false)));
    }

    #[test]
    fn dictionaries_compare_as_key_sets() {
        let a = PValue::Dict(vec![
            ("A".into(), PValue::Int(1)),
            ("B".into(), PValue::Int(2)),
        ]);
        let b = PValue::Dict(vec![
            ("B".into(), PValue::Int(2)),
            ("A".into(), PValue::Int(1)),
        ]);
        assert!(a.same(&b));
        let c = PValue::Dict(vec![("A".into(), PValue::Int(1))]);
        assert!(!a.same(&c));
        assert!(!c.same(&a));
    }
}

/// Integer/real equivalence is exact; finite ordinary integral values remain equivalent.
#[cfg(test)]
mod integer_real_boundary_tests {
    use super::PValue;

    #[test]
    fn integer_real_comparison_does_not_round_or_accept_small_fractions() {
        for (integer, real) in [
            (0, -0.0),
            (1, 1.0),
            (-7, -7.0),
            (i64::MIN, -9_223_372_036_854_775_808.0),
        ] {
            assert!(PValue::Int(integer).same(&PValue::Real(real)));
            assert!(PValue::Real(real).same(&PValue::Int(integer)));
        }
        assert!(PValue::Int(1).same(&PValue::Bool(true)));
        let distinct: Vec<bool> = [
            (0, f64::EPSILON / 2.0),
            (0, -f64::EPSILON / 2.0),
            (9_007_199_254_740_993, 9_007_199_254_740_992.0),
            (i64::MAX, 9_223_372_036_854_775_808.0),
            (i64::MIN + 1, -9_223_372_036_854_775_808.0),
            (0, f64::NAN),
            (i64::MAX, f64::INFINITY),
        ]
        .into_iter()
        .map(|(integer, real)| {
            !PValue::Int(integer).same(&PValue::Real(real))
                && !PValue::Real(real).same(&PValue::Int(integer))
        })
        .collect();
        assert_eq!(
            distinct, [true; 7],
            "both directions preserve exact integer identity"
        );
    }
}
