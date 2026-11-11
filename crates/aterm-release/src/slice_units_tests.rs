// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use aterm_capacity::book::Records;
use aterm_verify::unitgraph::UnitName;

const GIB: u64 = 1 << 30;
const SYNTHETIC_BUDGET: u64 = 42 * GIB;
const COMPLETE_BUDGET: u64 = 51 * GIB;
const NATIVE: &str = "aarch64-apple-darwin";
const COMPAT: &str = "x86_64-apple-darwin";

/// Fixture rows from the former lock-derived measurement identity.
fn key(triple: &str, unit: &UnitName) -> aterm_capacity::book::Key {
    measured_key(triple, unit, &toolchain_key(triple))
}

#[cfg(unix)]
#[test]
fn a_lock_is_not_evidence_of_the_compiler_that_ran() {
    use std::os::unix::fs::PermissionsExt;
    let dir = aterm_tempfile::tempdir().unwrap();
    let record = dir.path().join("units.json");
    let units = Units {
        record: record.clone(),
        triple: NATIVE.into(),
        budget: None,
    };
    let args = [
        "/usr/bin/true",
        "--crate-name",
        "fixture",
        "--crate-type",
        "lib",
    ]
    .map(OsString::from);
    assert_eq!(wrap(&args, Some("fixture"), &units, None), Ok(0));
    let records = Records::load(&record);
    assert!(
        !records
            .rows()
            .iter()
            .any(|row| row.key == key(NATIVE, &unit("fixture", "fixture lib"))),
        "a non-compiler was recorded as the source lock's Trust compiler"
    );
    assert!(
        records.rows()[0]
            .key
            .variant
            .as_deref()
            .unwrap()
            .ends_with(" unknown")
    );
    for (name, commit) in [("host", "aaaaaaaa"), ("compat", "bbbbbbbb")] {
        let program = dir.path().join(name);
        std::fs::write(&program, format!("#!/bin/sh\nfor arg do\nif [ \"$arg\" = -vV ]; then\n  printf 'rustc 1.99.0\\ncommit-hash: {commit}\\nhost: aarch64-apple-darwin\\n'\nfi\ndone\n")).unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut args = args.clone();
        args[0] = program.into_os_string();
        let identity = compiler_identity(&args);
        assert!(identity.contains(" compiler-vv-"));
        for triple in [NATIVE, COMPAT] {
            let mut units = units.clone();
            units.triple = triple.into();
            assert_eq!(wrap(&args, Some("fixture"), &units, None), Ok(0));
            assert!(
                peak_for_key(
                    &Records::load(&record),
                    &measured_key(triple, &unit("fixture", "fixture lib"), &identity)
                )
                .is_some()
            );
        }
    }
    // NEGATIVE CONTROL: one unchanged source lock cannot collapse different
    // actual compilers into one measurement row. Unknown history remains usable
    // only as a conservative same-metric admission estimate.
    let records = Records::load(&record);
    assert_eq!(records.rows().len(), 5);
    assert!(admission_peak(&records, NATIVE, &unit("fixture", "fixture lib")).is_some());
}

/// Cargo nests its workspace wrapper inside RUSTC_WRAPPER. Probe the same compiler
/// argument, including the sysroot selector, instead of asking the wrapper to be rustc.
#[cfg(unix)]
#[test]
fn fingerprint_preserves_workspace_wrapper_and_sysroot() {
    use std::os::unix::fs::PermissionsExt;
    let dir = aterm_tempfile::tempdir().unwrap();
    let wrapper = dir.path().join("workspace-wrapper");
    std::fs::write(&wrapper, "#!/bin/sh\n[ \"$1\" = compiler ] || exit 2\nshift\n[ \"$1\" = --crate-name ] || exit 6\nshift 2\n[ \"$1\" = --sysroot ] || exit 3\n[ \"$2\" = selected ] || exit 4\n[ \"$3\" = -vV ] || exit 5\nprintf 'rustc 1.99.0\\ncommit-hash: aaaaaaaa\\nhost: aarch64-apple-darwin\\n'\n").unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o700)).unwrap();
    let args = [
        wrapper.as_os_str().to_owned(),
        "compiler".into(),
        "--crate-name".into(),
        "fixture".into(),
        "--sysroot".into(),
        "selected".into(),
    ];
    assert!(compiler_identity(&args).contains(" compiler-vv-"));
    // NEGATIVE CONTROL: without the compiler argument, the wrapper rejects the query.
    assert!(compiler_identity(&[args[0].clone()]).ends_with(" unknown"));
}

/// An identity must hash a complete answer, not the tail of an oversized one.
#[cfg(unix)]
#[test]
fn fingerprint_rejects_truncated_version_answers() {
    use std::os::unix::fs::PermissionsExt;
    let dir = aterm_tempfile::tempdir().unwrap();
    let program = dir.path().join("large-version");
    std::fs::write(
        &program,
        "#!/bin/sh\nprintf '%1048577s\\n' x\nprintf 'commit-hash: aaaaaaaa\\n'\n",
    )
    .unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(compiler_identity(&[program.clone().into_os_string()]).ends_with(" unknown"));
    // NEGATIVE CONTROL: a complete small answer from the same executable is identified.
    std::fs::write(&program, "#!/bin/sh\nprintf 'commit-hash: aaaaaaaa\\n'\n").unwrap();
    assert!(compiler_identity(&[program.into_os_string()]).contains(" compiler-vv-"));
}

#[test]
fn an_unknown_compiler_row_remains_a_conservative_admission_estimate() {
    let unit = last_unit();
    let mut records = Records::default();
    let unknown = measured_key(NATIVE, &unit, &format!("{} unknown", accounting_name()));
    records.push(unknown, 40 * GIB, 1);
    records.push(key(NATIVE, &unit), GIB, 2);
    // NEGATIVE CONTROL: a smaller current row cannot hide the old unknown peak;
    // another metric cannot enlarge this metric's estimate.
    records.push(
        measured_key(NATIVE, &unit, "other-metric unknown"),
        99 * GIB,
        3,
    );
    assert_eq!(admission_peak(&records, NATIVE, &unit), Some(40 * GIB));
    assert_eq!(
        unit_need(NATIVE, &records, &unit, DRIVER).mem,
        claim_bytes(40 * GIB)
    );
}

#[test]
fn serial_parent_keeps_driver_room_inside_the_measured_compiler_margin() {
    // Sweep physical-footprint peaks around every GiB rounding boundary. Neither
    // a raw-peak claim nor a parent that forgets its driver satisfies this bound.
    for gib in 1..=96 {
        for offset in [0, 1, GIB / 5, GIB - 1] {
            let peak = gib * GIB + offset;
            let claim = claim_bytes(peak);
            let parent = serial_parent_bytes(claim);
            assert!(parent >= peak + DRIVER + FLOOR);
        }
    }
    // The parent reserves 5/6 of the pool plus the driver and the floor, not the pool:
    // 0.198.0 on m3 refused a 40 GiB parent on 39.7 GiB available, while the measured
    // 32.92 GiB link needs 35.92 with them.
    let m3 = serial_parent_bytes(40 * GIB);
    assert!(
        (3292 * GIB / 100 + DRIVER + FLOOR..37 * GIB).contains(&m3),
        "{m3}"
    );
    // Concurrent smaller units must also leave the driver and floor, rather than
    // relying only on the final link's individual measurement.
    for left in 1..=32 {
        for right in 1..=32 {
            let p = left * GIB;
            let q = right * GIB + 1;
            let pool = claim_bytes(p) + claim_bytes(q);
            assert!(serial_parent_bytes(pool) >= p + q + DRIVER + FLOOR);
        }
    }
}

#[cfg(unix)]
#[test]
/// Real 0.198.0 cut, 2026-11-10: degraded on m3, the serial wrapper refused `aterm-durable`,
/// `aterm-codec`, `aterm-hash` and `shlex`, which no earlier build had compiled under it, so a
/// degraded cut failed on any new crate. An unmeasured unit now claims the whole private pool
/// and compiles alone; known claims keep their size.
fn serial_margin_admits_an_unmeasured_unit_alone_and_preserves_known_claims() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let record = dir.path().join("units.json");
    let ledger_path = dir.path().join("ledger");
    let mut records = Records::default();
    records.push(key(NATIVE, &last_unit()), 33 * GIB, 1);
    records.save(&record).unwrap();
    let mut units = Units::of(&record, &[NATIVE], 1, &ledger_path);
    let before = unit_need(NATIVE, &records, &last_unit(), 40 * GIB).mem;
    use_serial_margin(&mut units);
    assert_eq!(before, 40 * GIB);
    assert_eq!(
        unit_need(NATIVE, &records, &last_unit(), 40 * GIB).mem,
        before
    );
    let ledger = aterm_capacity::Ledger::at(&ledger_path);
    let machine = BudgetMachine::new(40 * GIB);
    let args = [
        "/usr/bin/false",
        "--crate-name",
        "unmeasured",
        "--crate-type",
        "lib",
    ]
    .map(OsString::from);
    assert_eq!(
        unit_need(
            NATIVE,
            &records,
            &unit("unmeasured", "unmeasured lib"),
            40 * GIB
        )
        .mem,
        40 * GIB,
        "an unmeasured unit holds the whole private pool, so nothing compiles beside it"
    );
    let status = wrap(
        &args,
        Some("unmeasured"),
        &units[0],
        Some((&ledger, &machine)),
    )
    .expect("the serial wrapper starts an unmeasured unit's compiler");
    assert_eq!(status, 1, "the compiler ran: /usr/bin/false exits 1");
    assert!(ledger.list().unwrap().is_empty());
    let env = units[0].env(Path::new("wrapper"), dir.path());
    assert!(
        env.iter()
            .any(|(name, value)| *name == DRIVER_IN_MARGIN && value == "1")
    );
    let compiler = compiler(&args).unwrap();
    assert!(
        compiler
            .get_envs()
            .any(|(name, value)| name == DRIVER_IN_MARGIN && value.is_none())
    );
}

#[test]
fn compiler_and_metric_changes_keep_estimates_without_renaming_measurements() {
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
    for (index, variant) in variants.into_iter().enumerate() {
        let mut stale = current.clone();
        stale.variant = Some(variant);
        records.push(
            stale,
            match index {
                0 => 100 * GIB,
                1 => 2 * GIB,
                _ => 99 * GIB,
            },
            1,
        );
    }
    records.save(&record).unwrap();
    assert_eq!(peak_for_key(&records, &key(NATIVE, &last_unit())), None);
    let budget_ledger = dir.path().join("private-ledger");
    let units = Units::of(&record, &[NATIVE], 1, &budget_ledger);
    assert_eq!(units[0].budget_bytes(), Some(3 * GIB));
    assert!(!slice_need(NATIVE, Some(&units[0])).alone);
    let prior = admission_peak(&records, NATIVE, &last_unit()).unwrap();
    assert_eq!(prior, 2 * GIB);

    let other_target = key(COMPAT, &last_unit());
    let other_unit = key(NATIVE, &unit("aterm", "aterm bin_extra"));
    records.push(other_target, 99 * GIB, 1);
    records.push(other_unit, GIB, 1);
    assert_eq!(admission_peak(&records, NATIVE, &last_unit()), Some(prior));
    assert_eq!(
        peak_for_key(&records, &key(NATIVE, &last_unit())),
        None,
        "a prior compiler estimate is not a new exact measurement"
    );

    // A wrapper re-read refuses a parent that a newer compatible record has outgrown.
    #[cfg(unix)]
    {
        let mut claiming = units[0].clone();
        claiming.budget = Some(Budget {
            ledger: budget_ledger.clone(),
            bytes: MIN_BUDGET,
            driver_in_margin: false,
        });
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
            wrap(&args, Some("aterm"), &claiming, Some((&ledger, &machine)),)
                .unwrap_err()
                .contains("beyond the admitted")
        );
        assert!(ledger.list().unwrap().is_empty());
    }
    // A matching measurement receives the margin, retaining the larger run
    // when a later successful link is lighter.
    records.push(current.clone(), 20 * GIB, 2);
    records.push(current.clone(), 10 * GIB, 3);
    records.save(&record).unwrap();
    let learned = Units::of(&record, &[NATIVE], 1, &budget_ledger);
    assert_eq!(learned[0].budget_bytes(), Some(24 * GIB));
    assert_eq!(
        unit_need(NATIVE, &records, &last_unit(), DRIVER).mem,
        24 * GIB
    );
    let older = aterm_capacity::book::Key {
        variant: Some(format!("{prefix} trust-heavier")),
        ..current
    };
    records.push(older, 40 * GIB, 4);
    assert_eq!(
        peak_for_key(&records, &key(NATIVE, &last_unit())),
        Some(20 * GIB)
    );
    assert_eq!(
        unit_need(NATIVE, &records, &last_unit(), DRIVER).mem,
        claim_bytes(40 * GIB),
        "a smaller exact row cannot undercount a compatible recorded peak"
    );
}

/// SAME-METRIC PRIOR COMPILERS PROVISION THE SHARED PARENT BEFORE TREE WORK. Both 35.5-GB
/// final-link estimates become one 40-GiB private pool and a 44-GiB parent; exact current rows
/// remain absent until the compilers finish. NEGATIVE CONTROLS: another accounting metric cannot
/// authenticate the parent, and making both empty-history drivers ordinary would strand two
/// final-link ALONE requests behind the sibling drivers.
#[test]
fn prior_compilers_provision_one_parent_while_empty_history_stays_serial() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let record = dir.path().join("units.json");
    let private = dir.path().join("private-ledger");
    let mut records = Records::default();
    for triple in [NATIVE, COMPAT] {
        let mut old = key(triple, &last_unit());
        old.variant = Some(format!(
            "{triple} {} {} prior-compiler",
            last_unit().kind,
            accounting_metric(triple).unwrap()
        ));
        records.push(old, 35_506_777_064, 1);
    }
    records.save(&record).unwrap();

    let units = Units::of(&record, &[NATIVE, COMPAT], 2, &private);
    assert_eq!(units[0].budget_bytes(), Some(40 * GIB));
    assert_eq!(units[1].budget_bytes(), Some(40 * GIB));
    assert!(
        units
            .iter()
            .all(|units| peak_for_key(&records, &key(units.triple(), &last_unit())).is_none())
    );
    let parent = shared_need(&units, 2).unwrap();
    assert_eq!(parent.mem, 44 * GIB);

    let ledger = aterm_capacity::Ledger::at(dir.path().join("global-ledger"));
    let machine = aterm_capacity::Fixed::new(aterm_capacity::Reading::of_gib(128, 76, 18));
    let available = 52 * GIB;
    assert!(parent.mem <= available);
    assert!(
        parent.mem + 16 * GIB > available,
        "the tree must fit around, not consume, the reserved link budget"
    );
    let held = aterm_capacity::admit::try_admit_with(&ledger, &machine, parent)
        .unwrap()
        .expect("the build parent admits before tree work");
    drop(held);

    let mut incompatible = Records::default();
    for triple in [NATIVE, COMPAT] {
        let mut old = key(triple, &last_unit());
        old.variant = Some(format!("{triple} {} maxrss-v0 old", last_unit().kind));
        incompatible.push(old, 99 * GIB, 1);
    }
    incompatible.save(&record).unwrap();
    let cold = Units::of(&record, &[NATIVE, COMPAT], 2, &private);
    assert!(shared_need(&cold, 2).is_none());
    assert!(
        cold.iter()
            .all(|units| slice_need(units.triple(), Some(units)).alone)
    );

    let mutant_drivers = [NATIVE, COMPAT]
        .map(|triple| aterm_capacity::Need::new(format!("mutant {triple} driver")).mem(DRIVER));
    let final_links = [NATIVE, COMPAT].map(|triple| {
        aterm_capacity::Need::new(format!("mutant {triple} final link"))
            .mem(16 * GIB)
            .alone()
    });
    assert!(mutant_drivers.iter().all(|need| !need.alone));
    assert!(final_links.iter().all(|need| need.alone));
    assert!(
        mutant_drivers.iter().all(|need| need.mem > 0),
        "two ordinary held drivers would each block the sibling's final-link ALONE request"
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
        budget: None,
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
    assert!(started["memory_waited_for"].is_null());
    assert_eq!(ended["phase"].as_str(), Some("ended"));
    assert_eq!(ended["at_unix_s"].as_f64(), Some(20.0));
    assert_eq!(ended["cpu_s"].as_f64(), Some(6.25));
    assert_eq!(ended["success"].as_bool(), Some(false));
}

#[test]
fn unit_wait_evidence_keeps_the_resource_hosts_exact_holders() {
    let shortfall = aterm_capacity::Shortfall {
        shorts: Vec::new(),
        holders: vec![aterm_capacity::Holder {
            pid: 41,
            what: "release slice x86_64-apple-darwin unit aterm link".into(),
            checkout: "/release".into(),
            held: true,
            age: std::time::Duration::from_secs(7),
            mem: 22 * GIB,
            claimed: 23 * GIB,
            alone: false,
            disk: 0,
            quiet: false,
            order: (11, 41, 3),
            stalled: None,
            credit: Vec::new(),
            overrun: None,
        }],
    };
    let evidence = wait_evidence(Some(&shortfall));
    assert_eq!(evidence["holders"][0]["pid"].as_u64(), Some(41));
    assert_eq!(
        evidence["holders"][0]["what"].as_str(),
        Some("release slice x86_64-apple-darwin unit aterm link")
    );
    assert_eq!(
        evidence["holders"][0]["claimed_bytes"].as_u64(),
        Some(23 * GIB)
    );
    assert_eq!(
        evidence["holders"][0]["reserved_bytes"].as_u64(),
        Some(22 * GIB)
    );
    assert_eq!(
        evidence["holders"][0]["order"]["start_ms"].as_u64(),
        Some(11)
    );
    assert_eq!(evidence["holders"][0]["order"]["seq"].as_u64(), Some(3));
    assert!(
        wait_evidence(None)["holders"].is_null(),
        "NEGATIVE CONTROL: a unit that never waited must not acquire another wait's holders"
    );
}

#[cfg(unix)]
#[test]
fn wrapper_timeline_probe() {
    let Some(mode) = std::env::var_os("ATERM_SLICE_TIMELINE_PROBE") else {
        return;
    };
    let dir = aterm_tempfile::tempdir().unwrap();
    let ledger_path = dir.path().join("ledger");
    let units = Units {
        record: dir.path().join("record"),
        triple: NATIVE.to_owned(),
        budget: (mode == "claim").then_some(Budget {
            ledger: ledger_path.clone(),
            bytes: SYNTHETIC_BUDGET,
            driver_in_margin: false,
        }),
    };
    let mut records = Records::default();
    records.push(key(NATIVE, &last_unit()), GIB, 1);
    records.save(&units.record).unwrap();
    let mut args = vec![OsString::from("/usr/bin/false")];
    args.extend(["--crate-name", "fixture", "--crate-type", "lib"].map(OsString::from));
    if mode == "query" {
        args.push("--print=file-names".into());
    }
    let ledger = aterm_capacity::Ledger::at(&ledger_path);
    let machine = BudgetMachine::new(SYNTHETIC_BUDGET);
    let admission =
        (mode == "claim").then_some((&ledger, &machine as &dyn aterm_capacity::Machine));
    assert_eq!(wrap(&args, Some("fixture"), &units, admission), Ok(1));
}

/// The record the lock-root probe's wrapper admits against.
const LOCK_ROOT_PROBE_RECORD: &str = "ATERM_SLICE_LOCK_ROOT_PROBE_RECORD";

#[cfg(unix)]
#[test]
fn lock_root_probe() {
    let Some(record) = std::env::var_os(LOCK_ROOT_PROBE_RECORD) else {
        return;
    };
    let dir = aterm_tempfile::tempdir().unwrap();
    let ledger_path = dir.path().join("ledger");
    let units = Units {
        record: record.into(),
        triple: NATIVE.to_owned(),
        budget: Some(Budget {
            ledger: ledger_path.clone(),
            bytes: SYNTHETIC_BUDGET,
            driver_in_margin: false,
        }),
    };
    let args = Vec::from(
        [
            "/usr/bin/false",
            "--crate-name",
            "fixture",
            "--crate-type",
            "lib",
        ]
        .map(OsString::from),
    );
    let ledger = aterm_capacity::Ledger::at(&ledger_path);
    let machine = BudgetMachine::new(SYNTHETIC_BUDGET);
    let admission = Some((&ledger, &machine as &dyn aterm_capacity::Machine));
    println!(
        "lock-root-probe={:?}",
        wrap(&args, Some("fixture"), &units, admission)
    );
}

/// A REGISTRY PACKAGE'S COMPILER IS ADMITTED UNDER ITS SLICE'S TOOLCHAIN. Cargo starts it in the
/// package's own directory, which holds no `trust.lock`; the wrapper reads the lock under the
/// source root its parent names ([`ROOT`]) and finds the final link's retained footprint, so the
/// compiler runs. NEGATIVE CONTROL: without that root the same wrapper reads its working
/// directory, names no toolchain and refuses for want of a compatible footprint, which stopped
/// 0.198.0's first registry crate (`shlex`).
#[cfg(unix)]
#[test]
fn a_registry_packages_compiler_is_admitted_through_its_slices_named_root() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let record = dir.path().join("record");
    let mut records = Records::default();
    records.push(key(NATIVE, &last_unit()), GIB, 1);
    records.save(&record).unwrap();
    let registry = dir.path().join("registry-package");
    std::fs::create_dir(&registry).unwrap();
    let run = |root: Option<&Path>| {
        let mut command = aterm_uds::spawnfd::command(std::env::current_exe().unwrap());
        command
            .current_dir(&registry)
            .args([
                "--exact",
                "slice_units::tests::lock_root_probe",
                "--nocapture",
            ])
            .env(LOCK_ROOT_PROBE_RECORD, &record)
            .env_remove(ROOT);
        if let Some(root) = root {
            command.env(ROOT, root);
        }
        let output = command.output().unwrap();
        assert!(output.status.success(), "{output:?}");
        String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .find_map(|line| line.strip_prefix("lock-root-probe=").map(str::to_owned))
            .unwrap()
    };
    assert_eq!(run(Some(&aterm_tempfile::manifest_dir())), "Ok(1)");
    let refused = run(None);
    assert!(
        refused.contains("no compatible footprint for the final link"),
        "NEGATIVE CONTROL: {refused}"
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
    assert!(claimed[0]["memory_waited_for"].is_null());
    assert!(rows[1]["cpu_s"].as_f64().unwrap() >= 0.0);
    assert!(rows[1]["at_unix_s"].as_f64() >= rows[0]["at_unix_s"].as_f64());
    // Negative control: Cargo's compiler queries are not compiles, even with unit flags.
    assert!(run("query").is_empty());
}

/// AN UNIDENTIFIED NON-QUERY COMPILER SPENDS THE WHOLE PRIVATE BUDGET: missing unit flags and a
/// malformed unit shape both fail closed before the compiler starts. NEGATIVE CONTROL: the
/// injected machine has one GiB less than the private budget, so bypassing admission or claiming
/// any smaller amount runs `/usr/bin/true` and makes this test fail. Compiler queries remain the
/// only unidentified calls that bypass the private ledger (the timeline test above).
#[cfg(unix)]
#[test]
fn an_unidentified_non_query_compiler_claims_the_whole_private_budget() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let ledger_path = dir.path().join("ledger");
    let units = Units {
        record: dir.path().join("record"),
        triple: NATIVE.to_owned(),
        budget: Some(Budget {
            ledger: ledger_path.clone(),
            bytes: SYNTHETIC_BUDGET,
            driver_in_margin: false,
        }),
    };
    let mut records = Records::default();
    records.push(key(NATIVE, &last_unit()), GIB, 1);
    records.save(&units.record).unwrap();
    let ledger = aterm_capacity::Ledger::at(&ledger_path);
    let machine = BudgetMachine::new(SYNTHETIC_BUDGET - FLOOR);
    let cases: [Vec<OsString>; 2] = [
        vec!["/usr/bin/true".into()],
        ["/usr/bin/true", "--crate-name", "fixture", "--crate-type"]
            .map(OsString::from)
            .into(),
    ];
    for args in cases {
        let words = args
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert!(
            aterm_verify::memory::unit_of(words, Some("CARGO_PKG_NAME=fixture".to_owned()))
                .is_none(),
            "the negative control must remain unidentified"
        );
        assert!(
            wrap(&args, Some("fixture"), &units, Some((&ledger, &machine))).is_err(),
            "an unidentified compiler must claim all 42 GiB"
        );
        assert!(ledger.list().unwrap().is_empty());
    }
}

fn unit(package: &str, kind: &str) -> UnitName {
    UnitName {
        package: package.to_owned(),
        kind: kind.to_owned(),
    }
}

/// A COMPLETE EXACT-OR-COMPATIBLE RECORD GIVES BOTH SLICES ONE MARGINED UNIT BUDGET AND ONE PARENT
/// RESERVATION. The retained 41.7/22.8-GiB physical peaks produce 51/28-GiB unit claims, a
/// 51-GiB private pool and a 55-GiB two-driver parent. A later 44.1-GiB unit raises those to
/// 53/57 GiB. Cargo names the same private ledger and budget for each slice. NEGATIVE CONTROLS:
/// incomplete history learns alone; zero concurrency and inconsistent private budgets cannot
/// create a parent; an unknown unit takes the whole budget rather than an optimistic floor.
#[test]
fn a_complete_record_builds_one_shared_unit_budget_and_parent() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let record = dir.path().join("units.json");
    let ledger = dir.path().join("unit-budget");
    let slices = [NATIVE, COMPAT];
    let budgets = |at_once| {
        Units::of(&record, &slices, at_once, &ledger)
            .into_iter()
            .map(|units| units.budget_bytes())
            .collect::<Vec<_>>()
    };
    let empty = Units::of(&record, &slices, 2, &ledger);
    assert_eq!(budgets(2), [None, None]);
    assert!(shared_need(&empty, 2).is_none());
    assert!(slice_need(NATIVE, Some(&empty[0])).alone);
    assert!(slice_need(COMPAT, Some(&empty[1])).alone);
    assert!(slice_need(COMPAT, None).alone);
    assert_eq!(slice_need(NATIVE, Some(&empty[0])).mem, DRIVER);
    let wrapper = Path::new("/r/aterm-release");
    let root = Path::new("/private/take/snapshot/src");
    assert!(
        !empty[0]
            .env(wrapper, root)
            .iter()
            .any(|(key, _)| [BUDGET_LEDGER, BUDGET_BYTES].contains(key))
    );

    let mut records = Records::default();
    records.push(key(NATIVE, &last_unit()), 417 * GIB / 10, 1);
    records.push(
        key(NATIVE, &unit("aterm-gui", "aterm_gui lib")),
        192 * GIB / 10,
        1,
    );
    records.save(&record).unwrap();
    assert_eq!(
        budgets(2),
        [None, None],
        "the compat slice's last unit is not on record"
    );
    let mut records = Records::load(&record);
    records.push(key(COMPAT, &last_unit()), 228 * GIB / 10, 1);
    records.save(&record).unwrap();
    assert_eq!(budgets(2), [Some(COMPLETE_BUDGET), Some(COMPLETE_BUDGET)]);
    assert_eq!(budgets(1), [Some(COMPLETE_BUDGET), Some(COMPLETE_BUDGET)]);
    let both = Units::of(&record, &slices, 2, &ledger);
    assert!(both.iter().all(|units| units.budget_bytes().is_some()));
    assert_eq!(slice_need(NATIVE, Some(&both[0])).mem, DRIVER);
    assert_eq!(slice_need(COMPAT, Some(&both[1])).mem, DRIVER);
    assert!(!slice_need(NATIVE, Some(&both[0])).alone);
    assert_eq!(DRIVER, 2 * GIB);
    let parent = shared_need(&both, 2).unwrap();
    assert_eq!(parent.what, "release slice memory budget");
    assert_eq!(parent.rank, aterm_capacity::Rank::Release);
    assert_eq!(parent.mem, 55 * GIB);
    assert_eq!(parent.margin, 1);
    assert!(shared_need(&both, 0).is_none());
    let mut inconsistent = both.clone();
    inconsistent[1].budget.as_mut().unwrap().bytes += GIB;
    assert!(shared_need(&inconsistent, 2).is_none());
    let mut split = both.clone();
    split[1].budget.as_mut().unwrap().ledger = dir.path().join("mutant-split-budget");
    assert!(
        shared_need(&split, 2).is_none(),
        "one parent cannot bound slices whose wrappers spend different private ledgers"
    );

    let records = Records::load(&record);
    let link = unit_need(NATIVE, &records, &last_unit(), COMPLETE_BUDGET);
    assert_eq!(
        (link.mem, link.margin, link.leads, link.critical, link.rank,),
        (51 * GIB, 0, true, true, aterm_capacity::Rank::Release,)
    );

    let need = |unit: &UnitName| unit_need(NATIVE, &records, unit, COMPLETE_BUDGET);
    let gui = need(&unit("aterm-gui", "aterm_gui lib"));
    assert_eq!(gui.mem, 24 * GIB);
    assert!(gui.leads);
    assert!(!gui.critical, "only the slice's final link is critical");
    assert_eq!(gui.rank, aterm_capacity::Rank::Release);
    let unknown = need(&unit("aterm-cell", "aterm_cell lib"));
    assert_eq!(
        unknown.mem, COMPLETE_BUDGET,
        "an unknown unit consumes the whole private budget for its learning run"
    );
    assert!(unknown.leads);
    assert!(!unknown.critical);
    assert_eq!(unknown.rank, aterm_capacity::Rank::Release);
    let env = both[0].env(wrapper, root);
    let get = |k: &str| env.iter().find(|(n, _)| *n == k).map(|(_, v)| v.clone());
    assert_eq!(
        get("RUSTC_WRAPPER"),
        Some(wrapper.as_os_str().to_os_string())
    );
    assert_eq!(get(WRAPPER), Some(record.clone().into_os_string()));
    assert_eq!(get(TRIPLE), Some(NATIVE.into()));
    assert_eq!(get(BUDGET_LEDGER), Some(ledger.clone().into_os_string()));
    assert_eq!(get(BUDGET_BYTES), Some(COMPLETE_BUDGET.to_string().into()));
    assert_eq!(get(ROOT), Some(root.as_os_str().to_os_string()));

    let mut records = Records::load(&record);
    records.push(
        key(COMPAT, &unit("aterm-huge", "aterm_huge lib")),
        441 * GIB / 10,
        2,
    );
    records.save(&record).unwrap();
    let raised = Units::of(&record, &slices, 2, &ledger);
    assert_eq!(raised[0].budget_bytes(), Some(53 * GIB));
    assert_eq!(shared_need(&raised, 2).unwrap().mem, 57 * GIB);
}

#[cfg(target_os = "macos")]
#[test]
fn the_private_machine_keeps_process_identity_but_never_credits_real_pages() {
    let machine = BudgetMachine::new(COMPLETE_BUDGET);
    let procs = aterm_capacity::Machine::procs(&machine)
        .expect("the macOS process sweep must retain holder identity");
    let pid = std::process::id();
    assert!(
        procs.work(pid, &[], &[]).is_some(),
        "the private ledger must still know whether its holder is alive"
    );
    let use_ = procs.tree(pid);
    assert_eq!(use_.footprint, 0);
    assert_eq!(use_.charged, 0);
}

const BUDGET_PROBE_LEDGER: &str = "ATERM_SLICE_BUDGET_PROBE_LEDGER";
const BUDGET_PROBE_NEED: &str = "ATERM_SLICE_BUDGET_PROBE_NEED";

#[cfg(unix)]
#[test]
fn shared_budget_holder_probe() {
    use std::io::{Read as _, Write as _};

    let Some(path) = std::env::var_os(BUDGET_PROBE_LEDGER) else {
        return;
    };
    let bytes = std::env::var(BUDGET_PROBE_NEED)
        .unwrap()
        .parse::<u64>()
        .unwrap();
    let ledger = aterm_capacity::Ledger::at(Path::new(&path));
    let machine = BudgetMachine::new(COMPLETE_BUDGET);
    let _claim = aterm_capacity::admit_with(
        &ledger,
        &machine,
        aterm_capacity::Need::new("probe unit").mem(bytes),
        aterm_capacity::Admit::Wait,
        aterm_capacity::Pace::default(),
        &mut |_| {},
    )
    .unwrap();
    println!("slice-budget-held");
    std::io::stdout().flush().unwrap();
    let mut release = [0_u8];
    std::io::stdin().read_exact(&mut release).unwrap();
}

/// ONE PRIVATE LEDGER SERIALIZES THE TWO LINKS BUT PRESERVES SMALL-UNIT CONCURRENCY: while a
/// separate process holds 51 GiB, a 28 GiB request cannot enter the 51 GiB pool; while it holds
/// 8 GiB, a 7 GiB request can. NEGATIVE CONTROL: placing the second link in another ledger admits
/// it, demonstrating why every wrapper must inherit exactly the shared private path. The budget
/// machine retains live holder identity but supplies no process-memory credit against the fixed
/// pool.
#[cfg(unix)]
#[test]
fn one_private_budget_serializes_links_and_allows_smaller_units() {
    use std::io::Write as _;

    fn holder(path: &Path, need: u64) -> aterm_uds::spawnfd::Child {
        let mut command = aterm_uds::spawnfd::command(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "slice_units::tests::shared_budget_holder_probe",
                "--nocapture",
            ])
            .env(BUDGET_PROBE_LEDGER, path)
            .env(BUDGET_PROBE_NEED, need.to_string())
            .stdin(aterm_uds::spawnfd::Stdio::piped())
            .stdout(aterm_uds::spawnfd::Stdio::piped())
            .stderr(aterm_uds::spawnfd::Stdio::null());
        let mut child = command.spawn().unwrap();
        aterm_uds::deadline::prints(
            &mut child,
            "slice-budget-held",
            aterm_uds::deadline::ECHO_HANG,
        );
        child
    }

    fn release(mut child: aterm_uds::spawnfd::Child) {
        child.stdin.take().unwrap().write_all(b"\n").unwrap();
        assert!(child.wait().unwrap().success());
    }

    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join("shared");
    let ledger = aterm_capacity::Ledger::at(&path);
    let machine = BudgetMachine::new(COMPLETE_BUDGET);
    let native = holder(&path, COMPLETE_BUDGET);
    let compat = unit_need(
        COMPAT,
        &{
            let mut records = Records::default();
            records.push(key(COMPAT, &last_unit()), 23 * GIB, 1);
            records
        },
        &last_unit(),
        COMPLETE_BUDGET,
    );
    assert!(
        aterm_capacity::admit::try_admit_with(&ledger, &machine, compat.clone())
            .unwrap()
            .is_none(),
        "51 + 28 cannot spend a 51 GiB budget"
    );
    let split = aterm_capacity::Ledger::at(dir.path().join("mutant-separate-ledger"));
    let escaped = aterm_capacity::admit::try_admit_with(&split, &machine, compat)
        .unwrap()
        .expect("a second ledger loses the shared bound");
    drop(escaped);
    release(native);

    let small = holder(&path, 8 * GIB);
    let seven = aterm_capacity::Need::new("small unit").mem(7 * GIB);
    let admitted = aterm_capacity::admit::try_admit_with(&ledger, &machine, seven)
        .unwrap()
        .expect("8 + 7 fits the shared budget");
    drop(admitted);
    release(small);
    assert!(ledger.list().unwrap().is_empty());
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
    let mut removed: Vec<(String, bool)> = [
        DRIVER_IN_MARGIN,
        BUDGET_BYTES,
        BUDGET_LEDGER,
        WRAPPER,
        TRIPLE,
        ROOT,
        "RUSTC_WRAPPER",
    ]
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

/// THE WRAPPER RUNS EACH COMPILER UNDER ITS PRIVATE UNIT CLAIM AND KEEPS A FINISHED ONE'S PEAK:
/// a unit the record keeps gains this run's peak, exits as its compiler did, and its claim ends
/// with it; a query (`--print`) compiles nothing and keeps nothing. A new light unit starts a
/// floor-sized record, so its next run no longer takes the whole private budget, while a
/// previously retained unit keeps its later measured observations. NEGATIVE CONTROLS: a failed
/// compiler's peak is not kept; a unit above the admitted shared budget is refused rather than
/// escaping it through ALONE admission.
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
    records.save(&record).unwrap();
    let ledger_path = dir.path().join("ledger");
    let units = Units {
        record: record.clone(),
        triple: NATIVE.to_owned(),
        budget: Some(Budget {
            ledger: ledger_path.clone(),
            bytes: COMPLETE_BUDGET,
            driver_in_margin: false,
        }),
    };
    let ledger = aterm_capacity::Ledger::at(&ledger_path);
    let machine = BudgetMachine::new(COMPLETE_BUDGET);
    let run = |program: &str, name: &str, extra: &[&str]| {
        let mut args: Vec<OsString> = vec![program.into()];
        args.extend(["--crate-name", name, "--crate-type", "lib"].map(OsString::from));
        args.extend(extra.iter().map(OsString::from));
        wrap(&args, Some("fixture"), &units, Some((&ledger, &machine)))
    };
    let peaks = |u: &UnitName| {
        let records = Records::load(&record);
        let rows: Vec<_> = records
            .rows()
            .iter()
            .filter(|row| {
                row.key.crates.as_deref() == Some(std::slice::from_ref(&u.package))
                    && row
                        .key
                        .variant
                        .as_deref()
                        .is_some_and(|v| v.starts_with(&format!("{NATIVE} {} ", u.kind)))
            })
            .collect();
        (!rows.is_empty()).then(|| rows.iter().map(|row| row.peaks.len()).sum::<usize>())
    };
    assert_eq!(run("/usr/bin/true", "kept", &[]), Ok(0));
    assert_eq!(peaks(&kept), Some(2));
    assert!(ledger.list().unwrap().is_empty(), "its claim ended with it");
    assert_eq!(run("/usr/bin/true", "light", &[]), Ok(0));
    let light = unit("fixture", "light lib");
    assert_eq!(
        peaks(&light),
        Some(1),
        "a successful new light unit becomes known"
    );
    assert_eq!(
        admission_peak(&Records::load(&record), NATIVE, &light),
        Some(FLOOR),
        "its first observation is clamped to the retention floor"
    );
    assert_eq!(
        unit_need(NATIVE, &Records::load(&record), &light, COMPLETE_BUDGET).mem,
        2 * GIB,
        "the next run uses the margined floor instead of the whole private budget"
    );
    assert_eq!(run("/usr/bin/false", "kept", &[]), Ok(1));
    assert_eq!(peaks(&kept), Some(2), "a failed compile's peak is not kept");
    assert_eq!(
        run("/usr/bin/true", "kept", &["--print", "file-names"]),
        Ok(0)
    );
    assert_eq!(peaks(&kept), Some(2));

    Records::update(&record, |records| {
        records.push(key(NATIVE, &heavy), 43 * GIB, 1);
        true
    })
    .unwrap();
    let need = unit_need(NATIVE, &Records::load(&record), &heavy, COMPLETE_BUDGET);
    assert_eq!(need.mem, 52 * GIB);
    assert!(aterm_capacity::never(&need, &aterm_capacity::Machine::read(&machine, None)).is_some());
    assert!(run("/usr/bin/true", "heavy", &[]).is_err());
    assert_eq!(peaks(&heavy), Some(1));
    assert!(ledger.list().unwrap().is_empty());
}
