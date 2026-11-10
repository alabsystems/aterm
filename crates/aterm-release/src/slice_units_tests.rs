// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use aterm_capacity::book::Records;
use aterm_verify::unitgraph::UnitName;

const GIB: u64 = 1 << 30;
const NATIVE: &str = "aarch64-apple-darwin";
const COMPAT: &str = "x86_64-apple-darwin";

#[test]
fn stale_compiler_and_rss_records_cannot_admit_an_unmeasured_link() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let record = dir.path().join("units.json");
    let mut records = Records::default();
    let current = key(NATIVE, &last_unit());
    let prefix = current
        .variant
        .as_ref()
        .unwrap()
        .rsplit_once(' ')
        .unwrap()
        .0;
    let variants = vec![
        format!("{NATIVE} aterm bin"),
        format!("{prefix} trust-stale"),
    ];
    #[cfg(target_os = "macos")]
    let variants = {
        let mut variants = variants;
        variants.push(
            current
                .variant
                .as_ref()
                .unwrap()
                .replace("phys-footprint-tree-v1", "phys-footprint-v1"),
        );
        variants
    };
    for variant in variants {
        let mut stale = current.clone();
        stale.variant = Some(variant);
        records.push(stale, GIB, 1);
    }
    records.save(&record).unwrap();
    assert_eq!(peak(&records, NATIVE, &last_unit()), None);
    let units = Units::of(&record, &[NATIVE], 1);
    assert!(!units[0].claims());
    assert!(slice_need(NATIVE, Some(&units[0])).alone);
    // Even if the driver's earlier decision enabled unit claims, the wrapper
    // refuses before spawning a compiler against this stale book.
    #[cfg(unix)]
    {
        let mut claiming = units[0].clone();
        claiming.spare = Some(DRIVER);
        let args = [
            "/usr/bin/true",
            "--crate-name",
            "aterm",
            "--crate-type",
            "bin",
        ]
        .map(OsString::from);
        let ledger = aterm_capacity::Ledger::at(dir.path().join("ledger"));
        let machine = aterm_capacity::Fixed::new(aterm_capacity::Reading::of_gib(128, 76, 18));
        assert!(
            wrap(&args, Some("aterm"), &claiming, &ledger, &machine)
                .unwrap_err()
                .contains("no matching toolchain footprint")
        );
        assert!(ledger.list().unwrap().is_empty());
    }
    // A matching measurement receives the margin, retaining the larger run
    // when a later successful link is lighter.
    records.push(current.clone(), 20 * GIB, 2);
    records.push(current, 10 * GIB, 3);
    records.save(&record).unwrap();
    assert!(Units::of(&record, &[NATIVE], 1)[0].claims());
    assert_eq!(
        unit_need(NATIVE, &records, &last_unit(), DRIVER).mem,
        24 * GIB
    );
}

#[cfg(target_os = "macos")]
#[test]
fn unreaped_child_retains_its_kernel_footprint_peak() {
    let child = aterm_uds::spawnfd::command("/usr/bin/true")
        .spawn()
        .unwrap();
    let CompilerUsage {
        status, peak, rss, ..
    } = wait(child).unwrap();
    assert!(status.success());
    assert!(
        peak.is_some_and(|p| p > 0),
        "the final zombie read must supply physical footprint"
    );
    assert!(rss.is_some_and(|p| p > 0));
    // A missing process is not a zero-byte measurement.
    assert_eq!(footprint(u32::MAX), None);
}

#[test]
fn unit_timeline_keeps_identity_timestamps_failure_and_unknown_cpu() {
    let units = Units {
        record: PathBuf::from("unused"),
        triple: NATIVE.to_owned(),
        spare: None,
    };
    let unit = unit("aterm-gui", "aterm_gui lib");
    let started = unit_event(&units, &unit, "started", 10.0, 12.0, None, None);
    let ended = unit_event(&units, &unit, "ended", 10.0, 20.0, Some(6.25), Some(false));
    assert_eq!(started["event"].as_str(), Some("release-slice-unit"));
    assert_eq!(started["triple"].as_str(), Some(NATIVE));
    assert_eq!(started["package"].as_str(), Some("aterm-gui"));
    assert_eq!(started["unit"].as_str(), Some("aterm_gui lib"));
    assert_eq!(started["phase"].as_str(), Some("started"));
    assert_eq!(started["queued_unix_s"].as_f64(), Some(10.0));
    assert_eq!(started["at_unix_s"].as_f64(), Some(12.0));
    assert_eq!(started["pid"], ended["pid"]);
    // Negative controls: absence of a reaped CPU counter or exit is not zero or success.
    assert!(started["cpu_s"].is_null());
    assert!(started["success"].is_null());
    assert_eq!(ended["phase"].as_str(), Some("ended"));
    assert_eq!(ended["at_unix_s"].as_f64(), Some(20.0));
    assert_eq!(ended["cpu_s"].as_f64(), Some(6.25));
    assert_eq!(ended["success"].as_bool(), Some(false));
}

#[cfg(unix)]
#[test]
fn wrapper_timeline_probe() {
    let Some(mode) = std::env::var_os("ATERM_SLICE_TIMELINE_PROBE") else {
        return;
    };
    let dir = aterm_tempfile::tempdir().unwrap();
    let units = Units {
        record: dir.path().join("record"),
        triple: NATIVE.to_owned(),
        spare: (mode == "claim").then_some(2 * DRIVER),
    };
    let mut records = Records::default();
    records.push(key(NATIVE, &last_unit()), GIB, 1);
    records.save(&units.record).unwrap();
    let mut args = vec![OsString::from("/usr/bin/false")];
    args.extend(["--crate-name", "fixture", "--crate-type", "lib"].map(OsString::from));
    if mode == "query" {
        args.push("--print=file-names".into());
    }
    let ledger = aterm_capacity::Ledger::at(dir.path().join("ledger"));
    let machine = aterm_capacity::Fixed::new(aterm_capacity::Reading::of_gib(128, 76, 18));
    assert_eq!(
        wrap(&args, Some("fixture"), &units, &ledger, &machine),
        Ok(1)
    );
}

#[cfg(unix)]
#[test]
fn wrapper_retains_a_failed_units_timeline_but_does_not_invent_query_units() {
    let run = |mode| {
        let mut command = aterm_uds::spawnfd::command(std::env::current_exe().unwrap());
        command.args([
            "--exact",
            "slice_units::tests::wrapper_timeline_probe",
            "--nocapture",
        ]);
        command.env("ATERM_SLICE_TIMELINE_PROBE", mode);
        let output = command.output().unwrap();
        assert!(output.status.success(), "{output:?}");
        String::from_utf8(output.stderr)
            .unwrap()
            .lines()
            .filter_map(|line| aterm_json::from_str::<aterm_json::Value>(line).ok())
            .filter(|row| row["event"].as_str() == Some("release-slice-unit"))
            .collect::<Vec<_>>()
    };
    let rows = run("compile");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["phase"].as_str(), Some("started"));
    assert_eq!(rows[1]["phase"].as_str(), Some("ended"));
    assert_eq!(rows[1]["success"].as_bool(), Some(false));
    assert!(
        rows[0]["memory_waiting_s"].is_null(),
        "record-only mode admits no unit memory"
    );
    let claimed = run("claim");
    assert!(claimed[0]["memory_waiting_s"].as_f64().is_some());
    assert!(rows[1]["cpu_s"].as_f64().unwrap() >= 0.0);
    assert!(rows[1]["at_unix_s"].as_f64() >= rows[0]["at_unix_s"].as_f64());
    // Negative control: Cargo's compiler queries are not compiles, even with unit flags.
    assert!(run("query").is_empty());
}

fn unit(package: &str, kind: &str) -> UnitName {
    UnitName {
        package: package.to_owned(),
        kind: kind.to_owned(),
    }
}

/// Matching records enable unit claims with margins and driver spare. Without
/// both final links, the slices learn alone and wrappers only record.
#[test]
fn a_build_with_every_slices_last_unit_recorded_claims_its_drivers_and_each_unit_its_own() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let record = dir.path().join("units.json");
    let slices = [NATIVE, COMPAT];
    let spares = |at_once| {
        Units::of(&record, &slices, at_once)
            .into_iter()
            .map(|u| u.spare)
            .collect::<Vec<_>>()
    };
    let empty = Units::of(&record, &slices, 2);
    assert_eq!(spares(2), [None, None]);
    assert!(slice_need(NATIVE, Some(&empty[0])).alone);
    assert!(slice_need(COMPAT, Some(&empty[1])).alone);
    assert!(slice_need(COMPAT, None).alone);
    let wrapper = Path::new("/r/aterm-release");
    assert!(!empty[0].env(wrapper).iter().any(|(k, _)| *k == CLAIMS));

    let mut records = Records::default();
    records.push(key(NATIVE, &last_unit()), 417 * GIB / 10, 1);
    records.push(
        key(NATIVE, &unit("aterm-gui", "aterm_gui lib")),
        192 * GIB / 10,
        1,
    );
    records.save(&record).unwrap();
    assert_eq!(
        spares(2),
        [None, None],
        "the compat slice's last unit is not on record"
    );
    let mut records = Records::load(&record);
    records.push(key(COMPAT, &last_unit()), 228 * GIB / 10, 1);
    records.save(&record).unwrap();
    assert_eq!(spares(2), [Some(2 * DRIVER), Some(2 * DRIVER)]);
    assert_eq!(spares(1), [Some(DRIVER), Some(DRIVER)]);
    let both = Units::of(&record, &slices, 2);
    assert!(both.iter().all(Units::claims));
    assert_eq!(slice_need(NATIVE, Some(&both[0])).mem, DRIVER);
    assert_eq!(slice_need(COMPAT, Some(&both[1])).mem, DRIVER);
    assert_eq!(DRIVER, 2 * GIB);
    let records = Records::load(&record);
    let link = unit_need(NATIVE, &records, &last_unit(), 2 * DRIVER);
    assert_eq!(
        (link.mem, link.margin, link.leads, link.rank),
        (51 * GIB, 4 * GIB, true, aterm_capacity::Rank::Release)
    );
    let need = |u: &UnitName| unit_need(NATIVE, &records, u, 2 * DRIVER).mem;
    assert_eq!(need(&unit("aterm-gui", "aterm_gui lib")), 24 * GIB);
    assert_eq!(need(&unit("aterm-cell", "aterm_cell lib")), 2 * FLOOR);
    let env = both[0].env(wrapper);
    let get = |k: &str| env.iter().find(|(n, _)| *n == k).map(|(_, v)| v.clone());
    assert_eq!(
        get("RUSTC_WRAPPER"),
        Some(wrapper.as_os_str().to_os_string())
    );
    assert_eq!(get(WRAPPER), Some(record.into_os_string()));
    assert_eq!(get(TRIPLE), Some(NATIVE.into()));
    assert_eq!(get(CLAIMS), Some((4 * GIB).to_string().into()));
}

/// THE COMPILER SEES THE RELEASE'S ENVIRONMENT: the wrapper starts it with Cargo's words,
/// verbatim, and removes its own variables and `RUSTC_WRAPPER`, which a compiler's build would
/// otherwise inherit. NEGATIVE CONTROL: no other variable is touched (Cargo's `CARGO_PKG_NAME`
/// for the compiler among them).
#[test]
fn the_wrapper_starts_the_compiler_with_the_releases_environment() {
    let args: Vec<OsString> = ["/tc/rustc", "--crate-name", "aterm", "--crate-type", "bin"]
        .map(OsString::from)
        .into();
    let command = compiler(&args).unwrap();
    assert_eq!(command.get_program(), "/tc/rustc");
    assert_eq!(
        command.get_args().collect::<Vec<_>>(),
        args[1..]
            .iter()
            .map(OsString::as_os_str)
            .collect::<Vec<_>>()
    );
    let mut touched: Vec<(String, bool)> = command
        .get_envs()
        .map(|(k, v)| (k.to_string_lossy().into_owned(), v.is_none()))
        .collect();
    touched.sort();
    let mut removed: Vec<(String, bool)> = [CLAIMS, WRAPPER, TRIPLE, "RUSTC_WRAPPER"]
        .map(|k| (k.to_owned(), true))
        .into();
    removed.sort();
    assert_eq!(touched, removed);
    assert!(compiler(&[]).is_err());
}

/// THE COMPILER INHERITS THE DESCRIPTORS CARGO GAVE ITS WRAPPER: where Cargo made its own
/// jobserver it hands rustc its pipes as descriptors (`--jobserver-fds=4,6`), so the wrapper
/// starts the compiler as a pass-through. NEGATIVE CONTROL: an ordinary spawn
/// ([`aterm_uds::spawnfd::command`]) closes the same descriptor.
#[cfg(unix)]
#[test]
fn the_wrapper_passes_the_compiler_the_descriptors_cargo_gave_it() {
    use std::os::fd::{AsRawFd as _, FromRawFd as _};
    let file = std::fs::File::open("/dev/null").unwrap();
    // SAFETY: `file` is open; `F_DUPFD` returns a fresh descriptor without `FD_CLOEXEC`, or -1.
    let fd = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_DUPFD, 100) };
    assert!(fd >= 100, "F_DUPFD: {}", std::io::Error::last_os_error());
    // SAFETY: `fd` is the fresh descriptor this test alone owns.
    let held = unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) };
    // The shell exits 3 when it holds `fd`, else 0.
    let probe: Vec<OsString> = vec![
        "/bin/sh".into(),
        "-c".into(),
        aterm_uds::host::held_probe(fd).into(),
    ];
    let status = compiler(&probe).unwrap().status().unwrap();
    assert_eq!(status.code(), Some(3), "the compiler holds the descriptor");
    let mut plain = aterm_uds::spawnfd::command("/bin/sh");
    plain.args(&probe[1..]);
    assert_eq!(
        plain.status().unwrap().code(),
        Some(0),
        "an ordinary spawn closes it"
    );
    drop(held);
}

/// THE WRAPPER RUNS EACH COMPILER UNDER ITS UNIT'S CLAIM AND KEEPS A FINISHED ONE'S PEAK: a
/// unit the record keeps gains this run's peak, exits as its compiler did, and its claim ends
/// with it; a query (`--print`) compiles nothing and keeps nothing; a unit this host can never
/// hold beside its spare runs ALONE. NEGATIVE CONTROLS: a light unit the record does not keep
/// stays unkept; a compiler that fails exits as it did (1) and its peak, short of a finished
/// compile's, is not kept; the alone unit's own need is one the ledger refuses outright.
#[cfg(unix)]
#[test]
fn the_wrapper_runs_each_compiler_under_its_claim_and_keeps_a_finished_ones_peak() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let record = dir.path().join("units.json");
    let kept = unit("fixture", "kept lib");
    let heavy = unit("fixture", "heavy lib");
    let mut records = Records::default();
    records.push(key(NATIVE, &last_unit()), GIB, 1);
    records.push(key(NATIVE, &kept), 2 * GIB, 1);
    records.push(key(NATIVE, &heavy), 41 * GIB, 1);
    records.save(&record).unwrap();
    let units = Units {
        record: record.clone(),
        triple: NATIVE.to_owned(),
        spare: Some(2 * DRIVER),
    };
    let ledger = aterm_capacity::Ledger::at(dir.path().join("ledger"));
    let roomy = aterm_capacity::Fixed::new(aterm_capacity::Reading::of_gib(128, 76, 18));
    let run = |program: &str, name: &str, extra: &[&str], machine: &aterm_capacity::Fixed| {
        let mut args: Vec<OsString> = vec![program.into()];
        args.extend(["--crate-name", name, "--crate-type", "lib"].map(OsString::from));
        args.extend(extra.iter().map(OsString::from));
        wrap(&args, Some("fixture"), &units, &ledger, machine)
    };
    let peaks = |u: &UnitName| {
        let key = key(NATIVE, u);
        Records::load(&record)
            .rows()
            .iter()
            .find(|row| row.key == key)
            .map(|row| row.peaks.len())
    };
    assert_eq!(run("/usr/bin/true", "kept", &[], &roomy), Ok(0));
    assert_eq!(peaks(&kept), Some(2));
    assert!(ledger.list().unwrap().is_empty(), "its claim ended with it");
    assert_eq!(run("/usr/bin/true", "light", &[], &roomy), Ok(0));
    assert_eq!(peaks(&unit("fixture", "light lib")), None);
    assert_eq!(run("/usr/bin/false", "kept", &[], &roomy), Ok(1));
    assert_eq!(peaks(&kept), Some(2), "a failed compile's peak is not kept");
    assert_eq!(
        run("/usr/bin/true", "kept", &["--print", "file-names"], &roomy),
        Ok(0)
    );
    assert_eq!(peaks(&kept), Some(2));

    // 41 GiB with 4 spare is more than a 44 GiB host can ever hold; alone, it runs.
    let reading = aterm_capacity::Reading::of_gib(44, 40, 14);
    let need = unit_need(NATIVE, &Records::load(&record), &heavy, 2 * DRIVER);
    assert!(aterm_capacity::never(&need, &reading).is_some());
    let small = aterm_capacity::Fixed::new(reading);
    assert_eq!(run("/usr/bin/true", "heavy", &[], &small), Ok(0));
    assert_eq!(peaks(&heavy), Some(2));
    assert!(ledger.list().unwrap().is_empty());
}
