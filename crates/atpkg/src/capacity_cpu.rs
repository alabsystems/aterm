// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! CLI rendering of the shared read-only CPU lease observation.

use std::path::Path;

use aterm_capacity::{CpuHolder, CpuObservation};
use aterm_json::Value;

/// The command's presentation of the same facts native views consume.
pub(super) struct Report(CpuObservation);

impl Report {
    #[cfg(test)]
    pub(super) fn absent() -> Self {
        Self(if cfg!(unix) {
            CpuObservation::Absent
        } else {
            CpuObservation::Unsupported
        })
    }

    pub(super) fn read(dir: Option<&Path>) -> Self {
        Self(CpuObservation::read(dir))
    }

    pub(super) fn failed(&self) -> bool {
        matches!(&self.0, CpuObservation::Unreadable(_))
    }

    pub(super) fn lines(&self) -> Vec<String> {
        match &self.0 {
            CpuObservation::Unsupported => {
                vec!["cpu-leases unsupported on this platform".to_owned()]
            }
            CpuObservation::Absent => vec!["cpu-leases pool absent".to_owned()],
            CpuObservation::Initializing => {
                vec!["cpu-leases initializing; occupancy unavailable".to_owned()]
            }
            CpuObservation::Busy => {
                vec!["cpu-leases admission snapshot busy; occupancy unavailable".to_owned()]
            }
            CpuObservation::Unreadable(error) => vec![format!("cpu-leases unreadable: {error}")],
            CpuObservation::Ready {
                total,
                holders,
                external,
            } => {
                let mut lines = vec![format!(
                    "cpu-leases {} {}, {} available of {total}",
                    self.0.occupied().unwrap_or(0),
                    if external.is_some() {
                        "occupied"
                    } else {
                        "held"
                    },
                    self.0.available().unwrap_or(0),
                )];
                lines.extend(holders.iter().map(|holder| holder_line("", holder)));
                if let Some(external) = external {
                    lines.push(format!(
                        "  shared backing: {} native, {} external held, {} crash-retained",
                        holders.len(),
                        external.holders.len(),
                        external.retained.len()
                    ));
                    lines.extend(
                        external
                            .holders
                            .iter()
                            .map(|holder| holder_line("external ", holder)),
                    );
                    lines.extend(
                        external
                            .retained
                            .iter()
                            .map(|holder| holder_line("retained ", holder)),
                    );
                }
                lines
            }
        }
    }

    pub(super) fn json(&self) -> Value {
        self.0.json()
    }
}

fn holder_line(kind: &str, holder: &CpuHolder) -> String {
    format!(
        "  {kind}slot {} reported_pid={} reported_birth={}",
        holder.slot,
        holder
            .reported_pid
            .map_or_else(|| "unknown".to_owned(), |pid| pid.to_string()),
        holder
            .reported_birth
            .map_or_else(|| "unknown".to_owned(), |birth| birth.to_string())
    )
}

#[cfg(test)]
#[path = "capacity_cpu_tests.rs"]
mod tests;
