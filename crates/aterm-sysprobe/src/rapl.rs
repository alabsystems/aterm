// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Linux's CPU package energy: RAPL through powercap (`/sys/class/powercap/intel-rapl:N`, the
//! name AMD's driver gives its package zones too), the CPU watts A4 §4 names for Linux. Since
//! Linux 5.10 `energy_uj` is readable by root alone (CVE-2020-8694), so on most systems the first
//! read is refused; the reader keeps the refusal and never opens the file again.
//!
//! Each package zone (a top-level `intel-rapl:N` whose `name` is `package-N`, or `package-N-die-M`
//! where dies partition a package; not its `intel-rapl:N:M` subzones, which the package already
//! counts, and not the platform zone `psys`, top-level too, which counts the whole platform, the
//! packages included) is a microjoule counter that wraps at its `max_energy_range_uj`. Watts are
//! the zones' summed change over the time between two reads. The reader works under any root, so a
//! fixture tree tests it on every host; off Linux there are no zones and it reads absent.

use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::Why;

/// The most package zones read (one per socket, or per die where dies partition a package).
const MAX_ZONES: usize = 16;

/// The CPU packages' energy counters, read reading to reading.
#[derive(Debug)]
pub struct Rapl {
    zones: Vec<Zone>,
    /// The last read's time and counters, one per zone.
    last: Option<(Instant, Vec<u64>)>,
    /// Why every read answers nothing, once that is known for good.
    refused: Option<Why>,
}

#[derive(Debug)]
struct Zone {
    energy: PathBuf,
    range_uj: u64,
}

impl Rapl {
    /// The package zones under `root` (`/` on the machine), in zone order: those whose `name`
    /// says `package-`. With none, every read answers [`Why::Absent`].
    #[must_use]
    pub fn open(root: &Path) -> Self {
        let mut found: Vec<(u32, PathBuf)> = std::fs::read_dir(root.join("sys/class/powercap"))
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().into_string().ok()?;
                // A subzone's `N:M` does not parse as one number.
                let n = name.strip_prefix("intel-rapl:")?.parse().ok()?;
                // `psys`, the platform's zone, is top-level too and counts the packages again.
                crate::with_bounded(&e.path().join("name"), |t| t.trim().starts_with("package-"))
                    .filter(|package| *package)?;
                Some((n, e.path()))
            })
            .collect();
        found.sort_unstable();
        found.truncate(MAX_ZONES);
        let zones: Vec<Zone> = found
            .into_iter()
            .filter_map(|(_, dir)| {
                let range_uj = crate::with_bounded(&dir.join("max_energy_range_uj"), |t| {
                    crate::parse_uint::<u64>(t.trim())
                })
                .flatten()
                .filter(|r| *r > 0)?;
                Some(Zone {
                    energy: dir.join("energy_uj"),
                    range_uj,
                })
            })
            .collect();
        Self {
            refused: zones.is_empty().then_some(Why::Absent),
            zones,
            last: None,
        }
    }

    /// The packages' watts from the last read to this one, at `now`: `Ok(None)` at the first read
    /// and after one that failed, which leave no base.
    ///
    /// # Errors
    /// [`Why::Absent`] with no zone; [`Why::NeedsPrivilege`] (`root`) where a counter is root's
    /// alone, kept for every read after; [`Why::NotRead`] where a counter did not read this time.
    pub fn watts(&mut self, now: Instant) -> Result<Option<f64>, Why> {
        if let Some(why) = self.refused {
            return Err(why);
        }
        let mut read = Vec::with_capacity(self.zones.len());
        for zone in &self.zones {
            let mut buf = [0u8; 32];
            let uj = match crate::read_into(&zone.energy, &mut buf) {
                Err(std::io::ErrorKind::PermissionDenied) => {
                    self.refused = Some(Why::NeedsPrivilege("root"));
                    None
                }
                Err(_) => None,
                Ok((n, _)) => crate::parse_uint::<u64>(crate::utf8_prefix(&buf[..n]).trim()),
            };
            let Some(uj) = uj else {
                self.last = None;
                return Err(self.refused.unwrap_or(Why::NotRead));
            };
            read.push(uj);
        }
        let watts = self.last.as_ref().and_then(|(then, before)| {
            let secs = now.checked_duration_since(*then)?.as_secs_f64();
            let joules: f64 = self
                .zones
                .iter()
                .zip(before.iter().zip(&read))
                .map(|(z, (&a, &b))| {
                    // A counter wraps at its range: what ran to the top, then what ran from 0.
                    let uj = if b >= a {
                        b - a
                    } else {
                        z.range_uj.saturating_sub(a).saturating_add(b)
                    };
                    uj as f64 / 1e6
                })
                .sum();
            (secs > 0.0).then_some(joules / secs)
        });
        self.last = Some((now, read));
        Ok(watts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// A zone `dir` the kernel names `name` (its `name` file).
    fn zone(root: &Path, dir: &str, name: &str, energy: u64, range: u64) {
        let dir = root.join("sys/class/powercap").join(dir);
        std::fs::create_dir_all(&dir).expect("zone dir");
        std::fs::write(dir.join("name"), format!("{name}\n")).expect("name");
        std::fs::write(dir.join("energy_uj"), format!("{energy}\n")).expect("energy");
        std::fs::write(dir.join("max_energy_range_uj"), format!("{range}\n")).expect("range");
    }

    fn set(root: &Path, name: &str, energy: u64) {
        let path = root.join("sys/class/powercap").join(name).join("energy_uj");
        std::fs::write(path, format!("{energy}\n")).expect("energy");
    }

    /// Watts are the packages' summed change over the time between reads; the first read has no
    /// base; a counter that wrapped counts to its top and on from 0. CONTROLS: the package's
    /// subzone, which the package already counts, is not read (counted, it would add 5 W), and
    /// neither is the platform's top-level `psys`, which counts the packages again (12 W more).
    #[test]
    fn the_packages_watts_are_their_counters_change_over_the_time_between() {
        let root = aterm_tempfile::tempdir().expect("scratch root");
        let r = root.path();
        zone(r, "intel-rapl:0", "package-0", 1_000_000, 262_143_328_850);
        zone(r, "intel-rapl:0:0", "core", 500_000, 262_143_328_850);
        zone(r, "intel-rapl:1", "package-1", 9_000_000, 10_000_000);
        zone(r, "intel-rapl:2", "psys", 3_000_000, 262_143_328_850);
        let mut rapl = Rapl::open(r);
        let t0 = Instant::now();
        assert_eq!(rapl.watts(t0), Ok(None));
        set(r, "intel-rapl:0", 7_000_000);
        set(r, "intel-rapl:0:0", 10_500_000);
        set(r, "intel-rapl:1", 1_000_000);
        set(r, "intel-rapl:2", 27_000_000);
        let got = rapl.watts(t0 + Duration::from_secs(2)).expect("reads");
        // (6 J + 2 J wrapped) over 2 s.
        assert_eq!(got, Some(4.0));
    }

    /// A tree with no package zone (every system but Linux with powercap) reads absent.
    #[test]
    fn no_zone_reads_absent() {
        let root = aterm_tempfile::tempdir().expect("scratch root");
        assert_eq!(
            Rapl::open(root.path()).watts(Instant::now()),
            Err(Why::Absent)
        );
    }

    /// A counter root alone may read refuses every read after, without opening it again: once
    /// readable again it still answers the refusal. CONTROL: a counter that does not read once
    /// (gone, here) answers not read and reads again when it is back.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn a_root_only_counter_is_refused_for_good() {
        use std::os::unix::fs::PermissionsExt as _;
        let root = aterm_tempfile::tempdir().expect("scratch root");
        let r = root.path();
        zone(r, "intel-rapl:0", "package-0", 1_000_000, 262_143_328_850);
        let mut rapl = Rapl::open(r);
        let energy = r.join("sys/class/powercap/intel-rapl:0/energy_uj");
        std::fs::remove_file(&energy).expect("remove");
        assert_eq!(rapl.watts(Instant::now()), Err(Why::NotRead), "control");
        set(r, "intel-rapl:0", 2_000_000);
        assert_eq!(rapl.watts(Instant::now()), Ok(None), "control: read again");
        // SAFETY: geteuid takes nothing and cannot fail.
        if unsafe { libc::geteuid() } == 0 {
            // Root reads a file of mode 000: the refusal cannot be staged.
            return;
        }
        std::fs::set_permissions(&energy, std::fs::Permissions::from_mode(0o000)).expect("chmod");
        assert_eq!(rapl.watts(Instant::now()), Err(Why::NeedsPrivilege("root")));
        std::fs::set_permissions(&energy, std::fs::Permissions::from_mode(0o644)).expect("chmod");
        assert_eq!(rapl.watts(Instant::now()), Err(Why::NeedsPrivilege("root")));
    }
}
