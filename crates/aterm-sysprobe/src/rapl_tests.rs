// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
#[cfg(not(windows))]
use std::time::Duration;

/// A zone `dir` the kernel names `name` (its `name` file). Not on Windows: a zone's directory
/// name holds a colon (`intel-rapl:0`), which no file name there may, so the tests that write
/// one run where the reader does.
#[cfg(not(windows))]
fn zone(root: &Path, dir: &str, name: &str, energy: u64, range: u64) {
    let dir = root.join("sys/class/powercap").join(dir);
    std::fs::create_dir_all(&dir).expect("zone dir");
    std::fs::write(dir.join("name"), format!("{name}\n")).expect("name");
    std::fs::write(dir.join("energy_uj"), format!("{energy}\n")).expect("energy");
    std::fs::write(dir.join("max_energy_range_uj"), format!("{range}\n")).expect("range");
}

#[cfg(not(windows))]
fn set(root: &Path, name: &str, energy: u64) {
    let path = root.join("sys/class/powercap").join(name).join("energy_uj");
    std::fs::write(path, format!("{energy}\n")).expect("energy");
}

/// Watts are the packages' summed change over the time between reads; the first read has no
/// base; a counter that wrapped counts to its top and on from 0. CONTROLS: the package's
/// subzone, which the package already counts, is not read (counted, it would add 5 W), and
/// neither is the platform's top-level `psys`, which counts the packages again (12 W more).
#[cfg(not(windows))]
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
/// (gone, here) answers not read and reads again when it is back, with no base: the read before
/// the gap gives no watts over it.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_root_only_counter_is_refused_for_good() {
    use std::os::unix::fs::PermissionsExt as _;
    let root = aterm_tempfile::tempdir().expect("scratch root");
    let r = root.path();
    zone(r, "intel-rapl:0", "package-0", 1_000_000, 262_143_328_850);
    let mut rapl = Rapl::open(r);
    let t0 = Instant::now();
    assert_eq!(rapl.watts(t0), Ok(None), "the base");
    let energy = r.join("sys/class/powercap/intel-rapl:0/energy_uj");
    std::fs::remove_file(&energy).expect("remove");
    assert_eq!(
        rapl.watts(t0 + Duration::from_secs(1)),
        Err(Why::NotRead),
        "control"
    );
    set(r, "intel-rapl:0", 2_000_000);
    assert_eq!(
        rapl.watts(t0 + Duration::from_secs(2)),
        Ok(None),
        "control: read again, with no base"
    );
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
