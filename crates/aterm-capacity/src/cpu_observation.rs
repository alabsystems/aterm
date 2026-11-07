// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Read-only native CPU leases and crash-retained external reservations.
//! Counts observe shared backing; they never grant capacity or prove closure.

use aterm_json::{Map, Value};
use std::path::Path;

/// A slot's diagnostic record, not proof of process identity or release authority.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CpuHolder {
    /// Zero-based slot in its native or external domain.
    pub slot: u32,
    /// Reported PID, absent when a held record is unreadable.
    pub reported_pid: Option<u32>,
    /// Reported platform process-birth identity, if present.
    pub reported_birth: Option<u64>,
}

/// Validated external-domain occupancy, including unsettled debt after owner loss.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CpuExternal {
    /// Kernel-held external reservations, including unreadable owner records.
    pub holders: Vec<CpuHolder>,
    /// Unsettled reservations whose original kernel holder has closed.
    pub retained: Vec<CpuHolder>,
}
impl CpuExternal {
    /// All externally reserved units; neither process absence nor Drop releases them.
    #[must_use]
    pub fn occupied(&self) -> usize {
        self.holders.len().saturating_add(self.retained.len())
    }
    fn json(&self) -> Value {
        let mut out = Map::new();
        out.insert("state".into(), Value::from("active"));
        out.insert("held".into(), Value::from(self.holders.len() as u64));
        out.insert("retained".into(), Value::from(self.retained.len() as u64));
        out.insert("holders".into(), holders_json(&self.holders));
        out.insert("retained_holders".into(), holders_json(&self.retained));
        Value::Object(out)
    }
}

/// A nonblocking observation, never an admission entitlement.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CpuObservation {
    /// No implementation on this platform.
    Unsupported,
    /// No native pool in an unactivated legacy namespace.
    Absent,
    /// The native pool initializer currently holds its lock.
    Initializing,
    /// An admission transaction currently prevents a coherent observation.
    Busy,
    /// Unavailable, partial, malformed or inconsistent backing.
    Unreadable(String),
    /// Valid native occupancy and optional validated external backing.
    Ready {
        /// Sealed physical slot count.
        total: usize,
        /// Native kernel-held slots, including unreadable reported owners.
        holders: Vec<CpuHolder>,
        /// None only for unactivated legacy raw observation, never mixed readiness.
        external: Option<CpuExternal>,
    },
}
impl CpuObservation {
    /// Observed unavailable units, including live and crash-retained external debt.
    #[must_use]
    pub fn occupied(&self) -> Option<usize> {
        match self {
            Self::Ready {
                holders, external, ..
            } => Some(
                holders
                    .len()
                    .saturating_add(external.as_ref().map_or(0, CpuExternal::occupied)),
            ),
            _ => None,
        }
    }
    /// Observed unreserved backing, not a promise of CPU or memory admission.
    #[must_use]
    pub fn available(&self) -> Option<usize> {
        match self {
            Self::Ready { total, .. } => Some(total.saturating_sub(self.occupied()?)),
            _ => None,
        }
    }
    /// Machine-readable facts shared by authenticated control and local capacity.
    #[must_use]
    pub fn json(&self) -> Value {
        let mut out = Map::new();
        let state = match self {
            Self::Unsupported => "unsupported",
            Self::Absent => "absent",
            Self::Initializing => "initializing",
            Self::Busy => "busy",
            Self::Unreadable(error) => {
                out.insert("error".into(), Value::from(error.as_str()));
                "unreadable"
            }
            Self::Ready {
                total,
                holders,
                external,
            } => {
                out.insert("total".into(), Value::from(*total as u64));
                out.insert(
                    "held".into(),
                    Value::from(self.occupied().unwrap_or(0) as u64),
                );
                out.insert("native_held".into(), Value::from(holders.len() as u64));
                out.insert(
                    "available".into(),
                    Value::from(self.available().unwrap_or(0) as u64),
                );
                out.insert("holders".into(), holders_json(holders));
                let (basis, external) = match external {
                    Some(external) => ("shared_native_retained", external.json()),
                    None => ("raw_native", aterm_json::json!({"state":"unactivated"})),
                };
                out.insert("basis".into(), Value::from(basis));
                out.insert("external".into(), external);
                "ready"
            }
        };
        out.insert("state".into(), Value::from(state));
        Value::Object(out)
    }
    /// Read existing backing only. Adopted mixed namespaces require the sidecar;
    /// missing or invalid backing never appears as zero external reservation.
    #[must_use]
    pub fn read(dir: Option<&Path>) -> Self {
        #[cfg(unix)]
        {
            use crate::build_cpu::{LegacyRetirement, NativeDomain, Pool};
            let Some(dir) = dir else {
                return Self::Unreadable("account capacity directory unavailable".into());
            };
            let domain = match crate::user_namespace::resolve(dir) {
                Ok(Some(_)) => CpuDomain::Mixed,
                Ok(None) => CpuDomain::Legacy,
                Err(error) => return Self::Unreadable(error.to_string()),
            };
            if matches!(domain, CpuDomain::Mixed) {
                if crate::ledger::user_dir().as_deref() != Some(dir) {
                    return Self::Unreadable(
                        "mixed CPU observation requires the canonical account namespace".into(),
                    );
                }
                return match NativeDomain::open_canonical() {
                    Ok(native) => Self::read_pool(native.pool(), domain, Some(native.legacy())),
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        Self::Initializing
                    }
                    Err(error) => Self::Unreadable(error.to_string()),
                };
            }
            let pool = match Pool::open_existing(dir) {
                Ok(Some(pool)) => pool,
                Ok(None) if matches!(domain, CpuDomain::Legacy) => return Self::Absent,
                Ok(None) => {
                    return Self::Unreadable("mixed namespace has no native CPU pool".into());
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    return Self::Initializing;
                }
                Err(error) => return Self::Unreadable(error.to_string()),
            };
            let retirement = LegacyRetirement::open_canonical().ok();
            Self::read_pool(&pool, domain, retirement.as_ref())
        }
        #[cfg(not(unix))]
        {
            let _ = dir;
            Self::Unsupported
        }
    }
    #[cfg(unix)]
    fn read_pool(
        pool: &crate::build_cpu::Pool,
        domain: CpuDomain,
        retirement: Option<&crate::build_cpu::LegacyRetirement>,
    ) -> Self {
        use crate::build_cpu::{ExternalDebt, RetainedSlotState, SlotState};
        let (native, external) = match ExternalDebt::observe(pool, retirement) {
            Ok(rows) => rows,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return Self::Busy,
            Err(error) => return Self::Unreadable(error.to_string()),
        };
        if matches!(domain, CpuDomain::Mixed) && external.is_none() {
            return Self::Unreadable("mixed namespace has no external CPU backing".into());
        }
        let total = native.len();
        let holders = native
            .into_iter()
            .filter_map(|slot| match slot {
                SlotState::Available { .. } => None,
                SlotState::Held {
                    slot,
                    reported_owner,
                } => Some(holder(slot, reported_owner)),
            })
            .collect();
        let external = external.map(|slots| {
            let mut out = CpuExternal::default();
            for slot in slots {
                match slot {
                    RetainedSlotState::Available { .. } => (),
                    RetainedSlotState::Held {
                        slot,
                        reported_owner,
                    } => out.holders.push(holder(slot, reported_owner)),
                    RetainedSlotState::Retained {
                        slot,
                        reported_owner,
                    } => out.retained.push(holder(slot, Some(reported_owner))),
                }
            }
            out
        });
        let value = Self::Ready {
            total,
            holders,
            external,
        };
        if value.occupied().is_some_and(|occupied| occupied > total) {
            return Self::Unreadable("native and retained CPU occupancy exceeds backing".into());
        }
        value
    }
}
#[cfg(unix)]
#[derive(Clone, Copy)]
enum CpuDomain {
    Legacy,
    Mixed,
}
#[cfg(unix)]
fn holder(slot: u32, owner: Option<crate::build_cpu::Owner>) -> CpuHolder {
    CpuHolder {
        slot,
        reported_pid: owner.map(|owner| owner.pid),
        reported_birth: owner.and_then(|owner| owner.birth),
    }
}
fn holders_json(holders: &[CpuHolder]) -> Value {
    Value::Array(
        holders
            .iter()
            .map(|holder| {
                let mut row = Map::new();
                row.insert("slot".into(), Value::from(holder.slot));
                row.insert("reported_pid".into(), Value::from(holder.reported_pid));
                row.insert("reported_birth".into(), Value::from(holder.reported_birth));
                Value::Object(row)
            })
            .collect(),
    )
}

#[cfg(test)]
mod wire_tests {
    use super::*;

    #[test]
    fn unavailable_observations_never_advertise_free_slots() {
        for observation in [
            CpuObservation::Unsupported,
            CpuObservation::Absent,
            CpuObservation::Initializing,
            CpuObservation::Busy,
            CpuObservation::Unreadable("permission denied".into()),
        ] {
            let json = observation.json();
            for field in ["total", "held", "available", "holders"] {
                assert!(json.get(field).is_none(), "{json}");
            }
        }
    }

    #[test]
    fn held_lock_without_owner_record_still_consumes_capacity() {
        let json = CpuObservation::Ready {
            total: 2,
            external: None,
            holders: vec![CpuHolder {
                slot: 1,
                reported_pid: None,
                reported_birth: None,
            }],
        }
        .json();
        assert_eq!(json["held"], Value::from(1u64));
        assert_eq!(json["available"], Value::from(1u64));
        assert_eq!(json["holders"][0]["reported_pid"], Value::Null);
        assert_eq!(json["holders"][0]["reported_birth"], Value::Null);
    }
}

#[cfg(all(test, unix))]
#[path = "cpu_observation/tests.rs"]
mod tests;

#[cfg(all(test, not(unix)))]
mod unsupported_tests {
    use super::CpuObservation;

    #[test]
    fn unsupported_platform_is_not_reported_as_an_empty_pool() {
        assert_eq!(CpuObservation::read(None), CpuObservation::Unsupported);
    }
}
