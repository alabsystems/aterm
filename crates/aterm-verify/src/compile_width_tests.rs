// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::memory::StageKey;

#[cfg(unix)]
#[test]
fn the_real_lint_stage_installs_pool_flags_without_an_outer_wrapper() {
    use crate::{Check, Ctx, EnvSnapshot, Scope};
    thread_local! { static LEDGER: RefCell<Option<PathBuf>> = const { RefCell::new(None) }; }
    fn admit(
        need: aterm_capacity::Need,
        mode: aterm_capacity::Admit,
    ) -> Result<aterm_capacity::Claim, aterm_capacity::Error> {
        assert_eq!(need.mem, aterm_capacity::cold_claim(Some(128 << 30)));
        assert!(need.alone, "a lint never measured here asks to run alone");
        assert_eq!(mode, aterm_capacity::Admit::Wait);
        LEDGER.with(|path| {
            aterm_capacity::admit_with(
                &aterm_capacity::Ledger::at(path.borrow().as_ref().unwrap()),
                &aterm_capacity::Fixed::new(aterm_capacity::Reading::of_gib(128, 76, 18)),
                need,
                mode,
                aterm_capacity::Pace::default(),
                &mut |_| {},
            )
        })
    }

    let dir = aterm_tempfile::tempdir().unwrap();
    LEDGER.with(|path| path.replace(Some(dir.path().join("ledger"))));
    let mut ctx = Ctx::new(
        dir.path().to_owned(),
        Scope::crate_only("aterm-verify"),
        EnvSnapshot::default(),
        dir.path().join("scratch"),
    )
    .with_checks(&[Check::Lint]);
    ctx.pool = Some(
        aterm_capacity::Jobserver::new(&dir.path().join("jobs"), 4)
            .unwrap()
            .into(),
    );
    ctx.env.cargo_build_jobs = Some("3".into());
    let memory = dir.path().join("memory.json");
    ctx.memory = Some(crate::memory::Book::at(
        Some(memory.clone()),
        Some(128 << 30),
        Some(120 << 30),
    ));
    ctx.unit_reader = |_| crate::memory::Named::Nothing;
    let mut spec = crate::plan::plan(&ctx)
        .into_iter()
        .find(|s| s.id == crate::plan::StageId::Tippy)
        .unwrap();
    // Force the stage estimate light; the lint was never measured here, so it still claims the
    // cold figure and runs alone, while its observation belongs to the pooled width.
    spec.mem = 0;
    let quiet = crate::Quiet::of(&ctx, std::slice::from_ref(&spec), crate::LIVE.admit_quiet);
    let early = crate::EarlyClaims::default();
    let observed = std::cell::Cell::new(false);
    let seams = crate::Seams {
        admit_memory: admit,
        ..crate::LIVE
    };
    let _ = crate::staged(&ctx, &quiet, &spec, None, seams, &early, || {
        assert_eq!(current(), Some(3));
        let pool = ctx.pool.as_ref().unwrap();
        let mut command = aterm_uds::spawnfd::command("unused");
        configure(&mut command);
        assert!(
            command
                .get_envs()
                .any(|(k, v)| k == "MAKEFLAGS" && v == Some(std::ffi::OsStr::new(&pool.flags())))
        );
        assert!(
            !command
                .get_envs()
                .any(|(name, _)| { name == "RUSTC_WRAPPER" || name == UNIT_MODE })
        );
        let tokens: Vec<_> = (0..3)
            .map(|_| pool.try_acquire().unwrap().unwrap())
            .collect();
        assert!(pool.try_acquire().unwrap().is_none());
        drop(tokens);
        let meter = crate::memory::current().unwrap();
        meter.started();
        meter.book(
            123,
            &[aterm_sysprobe::MemberUsage {
                pid: 123,
                start: 1,
                cpu_ns: 1,
                runnable_ns: 1,
                io_bytes: 0,
                footprint: 8 << 20,
                peak_footprint: 8 << 20,
            }],
        );
        meter.ended(123);
        observed.set(true);
        let mut report = crate::Report::new("lint");
        report.pass("ran with the shared pool");
        report
    });
    assert!(observed.get(), "the lint callback must actually run");
    assert_eq!(current(), None);
    let ordinary = crate::memory::Key::of_stage(&spec, &ctx);
    let pooled = record_key(ordinary.clone(), Some(3));
    let records = crate::memory::Records::load(&memory);
    assert!(records.rows().iter().any(|row| row.key == pooled));
    assert!(!records.rows().iter().any(|row| row.key == ordinary));
}

#[cfg(unix)]
#[test]
fn lint_without_per_unit_memory_claims_still_borrows_from_the_release_fifo() {
    use crate::plan::StageId;
    let dir = aterm_tempfile::tempdir().unwrap();
    let pool = aterm_capacity::Jobserver::new(&dir.path().join("jobs"), 4).unwrap();
    for (stage, width) in [
        (StageId::Tippy, 3),
        (StageId::Doctests, 3),
        (StageId::SpecCheck, 3),
        (StageId::TestCompile, 3),
        (StageId::ExportScan, 1),
    ] {
        let jobs = stage_jobs(stage, Some(pool.grant()), Some(3));
        assert_eq!(jobs, Some(width));
        run(Some(&pool), jobs, None, &|| false, || {
            let mut command = aterm_uds::spawnfd::command("unused");
            configure(&mut command);
            assert!(
                command
                    .get_envs()
                    .any(|(key, value)| key == "CARGO_MAKEFLAGS"
                        && value == Some(std::ffi::OsStr::new(&pool.flags())))
            );
            let tokens: Vec<_> = (0..3)
                .map(|_| pool.try_acquire().unwrap().unwrap())
                .collect();
            assert!(
                pool.try_acquire().unwrap().is_none(),
                "the stage owns its implicit token"
            );
            drop(tokens);
        })
        .unwrap();
    }
    // Source-worker widths remain in memory keys; formatting's coordinator has no width.
    assert_eq!(stage_jobs(StageId::Formatting, Some(4), None), None);
    assert_eq!(stage_jobs(StageId::Census, Some(4), None), Some(1));
    assert_eq!(stage_jobs(StageId::Forge, Some(4), None), Some(1));
    assert_eq!(stage_jobs(StageId::Tippy, None, Some(4)), None);
    assert_eq!(stage_jobs(StageId::TestCompile, Some(4), Some(2)), Some(2));
}

#[cfg(unix)]
#[test]
fn cargo_width_uses_the_eventual_grant_while_the_fifo_starts_at_one() {
    use crate::plan::StageId;
    let dir = aterm_tempfile::tempdir().unwrap();
    let shares =
        aterm_capacity::BranchShares::new(&dir.path().join("qualify.jobs"), 14, 0, [true, true])
            .unwrap();
    let tree =
        aterm_capacity::Jobserver::adopt(&shares.pool(true).handed_share(1).unwrap()).unwrap();
    assert_eq!(tree.slots(), 1);
    assert_eq!(tree.grant(), 14);
    let mut ctx = crate::Ctx::new(
        dir.path().to_owned(),
        crate::Scope::crate_only("aterm-verify"),
        crate::EnvSnapshot::default(),
        dir.path().join("scratch"),
    )
    .with_pool(Some(tree));
    assert_eq!(
        jobs_for(&ctx, StageId::Tippy),
        Some(14),
        "the production selector reads the eventual grant, not the initial allocation"
    );
    ctx.env.cargo_build_jobs = Some("3".into());
    assert_eq!(jobs_for(&ctx, StageId::Tippy), Some(3));
    assert_eq!(stage_jobs(StageId::Tippy, Some(14), None), Some(14));
    assert_eq!(
        stage_jobs(StageId::Tippy, Some(14), Some(3)),
        Some(3),
        "an explicit caller ceiling still narrows Cargo"
    );
    assert_ne!(
        stage_jobs(StageId::Tippy, Some(1), None),
        Some(14),
        "NEGATIVE CONTROL: freezing Cargo at the initial allocation prevents later widening"
    );
}

#[test]
fn test_compile_unit_envelopes_match_the_package_and_compiler_kind() {
    let mut records = crate::memory::Records::default();
    let unit = crate::unitgraph::UnitName {
        package: "aterm-gui".into(),
        kind: "aterm_gui test".into(),
    };
    let key = crate::shape::compiler_unit_key(crate::shape::UNIT_STAGE, "/old", &unit);
    records.push(key, 196 * (1 << 30) / 10, 1);
    assert_eq!(
        unit_peak(&records, crate::shape::UNIT_STAGE, &unit),
        Some(196 * (1 << 30) / 10)
    );
    let lib = crate::unitgraph::UnitName {
        kind: "aterm_gui lib".into(),
        ..unit.clone()
    };
    assert_eq!(unit_peak(&records, crate::shape::UNIT_STAGE, &lib), None);
    let other = crate::unitgraph::UnitName {
        package: "other".into(),
        ..unit
    };
    assert_eq!(
        unit_peak(&records, crate::shape::UNIT_STAGE, &other),
        None,
        "NEGATIVE CONTROL: an absent unit never falls back to the old 1 GiB default"
    );

    let malformed = crate::memory::Key {
        stage: crate::shape::UNIT_STAGE.into(),
        checkout: "/old".into(),
        crates: Some(Vec::new()),
        variant: Some(String::new()),
    };
    let mut malformed_records = crate::memory::Records::default();
    malformed_records.push(malformed, 9 << 30, 2);
    assert!(
        !has_units(&malformed_records, crate::shape::UNIT_STAGE),
        "NEGATIVE CONTROL: a malformed row cannot mint the 2 GiB driver"
    );
}

#[test]
fn attributed_footprint_wins_over_a_lower_wrapper_maxrss() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let stage_book = dir.path().join("stage-memory.json");
    let unit_book = unit_book(&stage_book, crate::plan::StageId::TestCompile);
    let unit = crate::unitgraph::UnitName {
        package: "aterm-gpu".into(),
        kind: "aterm_gpu lib".into(),
    };
    let failed = Units {
        book: unit_book.clone(),
        checkout: "/fixture".into(),
        mode: UnitMode::Record,
        pending: dir.path().join("failed.pending.json"),
        run: "failed".into(),
        stage: crate::shape::UNIT_STAGE.into(),
        unknown: aterm_capacity::COLD_CLAIM,
        wrapper: PathBuf::from("/fixture/aterm-verify"),
    };
    keep_unit(&failed, &unit, 12 << 30).unwrap();
    assert_eq!(
        unit_peak(
            &crate::memory::Records::load(&unit_book),
            crate::shape::UNIT_STAGE,
            &unit,
        ),
        None,
        "NEGATIVE CONTROL: a successful compiler inside a failed stage is only pending and cannot enable driver admission"
    );

    let passed = Units {
        pending: dir.path().join("passed.pending.json"),
        run: "passed".into(),
        ..failed.clone()
    };
    keep_unit(&passed, &unit, 256 << 20).unwrap();
    assert_eq!(
        unit_peak(
            &crate::memory::Records::load(&passed.pending),
            crate::shape::UNIT_STAGE,
            &unit,
        ),
        Some(crate::memory::UNIT_FLOOR)
    );
    let footprint = 9 << 30;
    let meter_only = crate::unitgraph::UnitName {
        package: "failed-compiler".into(),
        kind: "failed_compiler lib".into(),
    };
    let peaks = std::collections::BTreeMap::from([
        (unit.clone(), footprint),
        (meter_only.clone(), 10 << 30),
    ]);
    let book = crate::memory::Book::at(Some(stage_book), Some(128 << 30), Some(76 << 30));
    assert_eq!(
        keep_meter_units(&book, crate::plan::StageId::TestCompile, &passed, &peaks,).unwrap(),
        1
    );
    keep_unit(&passed, &unit, 256 << 20).unwrap();
    assert_eq!(
        unit_peak(
            &crate::memory::Records::load(&unit_book),
            crate::shape::UNIT_STAGE,
            &unit,
        ),
        Some(footprint),
        "NEGATIVE CONTROL: a lower wait4 maxrss cannot replace the meter's lifetime footprint"
    );
    assert_eq!(
        unit_peak(
            &crate::memory::Records::load(&unit_book),
            crate::shape::UNIT_STAGE,
            &meter_only,
        ),
        None,
        "NEGATIVE CONTROL: an attributed process without a successful wrapper completion is not proven"
    );
    discard_pending(&failed).unwrap();
    discard_pending(&passed).unwrap();
    assert!(!failed.pending.exists() && !passed.pending.exists());
}

#[test]
fn pooled_peaks_cannot_replace_unpooled_history() {
    let key = crate::memory::Key {
        stage: "TestCompile".into(),
        checkout: "/tree".into(),
        crates: None,
        variant: None,
    };
    let pooled = record_key(key.clone(), Some(14));
    assert_ne!(key, pooled);
    assert_eq!(record_key(key.clone(), None), key);
}

#[cfg(unix)]
#[test]
fn compile_borrows_one_implicit_token_and_can_widen_from_the_same_pool() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let pool = aterm_capacity::Jobserver::new(&dir.path().join("jobs"), 4).unwrap();
    let slice = pool.acquire(&|| false).unwrap();
    run(Some(&pool), Some(3), None, &|| false, || {
        let mut command = aterm_uds::spawnfd::command("unused");
        configure(&mut command);
        assert!(
            command
                .get_envs()
                .any(|(key, value)| key == "CARGO_MAKEFLAGS"
                    && value == Some(std::ffi::OsStr::new(&pool.flags())))
        );
        let first = pool.try_acquire().unwrap().unwrap();
        let second = pool.try_acquire().unwrap().unwrap();
        assert!(pool.try_acquire().unwrap().is_none());
        drop(slice);
        let widening = pool.try_acquire().unwrap().unwrap();
        assert!(pool.try_acquire().unwrap().is_none());
        drop((first, second, widening));
    })
    .unwrap();
    assert_eq!(current(), None);
    let tokens: Vec<_> = (0..4)
        .map(|_| pool.try_acquire().unwrap().unwrap())
        .collect();
    assert!(pool.try_acquire().unwrap().is_none());
    drop(tokens);
}

/// Deterministic event model, not a timing assertion about this host. The compiler has
/// 4,205 CPU-s, the design's measurement, with a 19.6 GiB late unit and a long 1 GiB tail.
/// Its full-width critical chain is 450s. Native/compat end at 1251/1635s if admitted at zero;
/// remaining tree work is the measured 1805s tree minus its modelled 450s compile.
#[test]
fn m3_unit_admission_beats_the_fixed_width_negative_control() {
    use aterm_capacity::{Fixed, Ledger, Machine as _, Need, Reading};
    const GIB: u64 = 1 << 30;
    const ROOM: u64 = 758 * GIB / 10;
    const CEILING: u32 = 14; // 18-core Release grant, export's four tokens held separately.
    let simulate = |native_gib: u64, compat_gib: u64, per_unit: bool, limit: u32| {
        let dir = aterm_tempfile::tempdir().unwrap();
        let ledger = Ledger::at(dir.path());
        let machine = Fixed::new(Reading::of_gib(128, 76, 18));
        let mut reading = machine.read(None);
        reading.available = Some(ROOM);
        machine.set(reading);
        let ask = |name: &str, bytes| {
            aterm_capacity::admit::try_admit_with(
                &ledger,
                &machine,
                Need::new(name).mem(bytes).margin(1),
            )
            .unwrap()
        };
        let mut native = Some(ask("native", native_gib * GIB).unwrap());
        let mut compat = Some(ask("compat", compat_gib * GIB).unwrap());
        let peaks: Vec<_> = std::iter::once(196 * GIB / 10)
            .chain(std::iter::once(47 * GIB / 10))
            .chain(std::iter::repeat_n(GIB, 12))
            .collect();
        // Removed implementation, kept here only as the negative-control policy.
        let widths: Vec<_> = (1..=CEILING)
            .rev()
            .map(|jobs| {
                (
                    jobs,
                    aterm_capacity::peak_claim(GIB + peaks.iter().take(jobs as usize).sum::<u64>()),
                )
            })
            .collect();
        let mut driver = None;
        let mut width = 0;
        let mut jobs: Vec<(u32, u64)> = vec![(20, GIB); 189];
        // Thirteen full 14-job batches, then seven half-length jobs: 270s at 14.
        for job in &mut jobs[182..] {
            job.0 = 10;
        }
        jobs[0].1 = 47 * GIB / 10;
        let mut tail = vec![(180, 196 * GIB / 10)];
        tail.extend(std::iter::repeat_n((20, GIB), 15));
        tail.push((15, GIB));
        assert_eq!(jobs.iter().chain(&tail).map(|(s, _)| s).sum::<u32>(), 4205);
        let mut active: Vec<(u32, Option<aterm_capacity::Claim>)> = Vec::new();
        let mut phase_one = false;
        let mut admitted_at = None;
        let mut widest_before_release = 0;
        let mut second = 0;
        for _ in 0..1000 {
            if second == 1251 {
                drop(native.take());
            }
            if second == 1635 {
                drop(compat.take());
            }
            active.retain(|(end, _)| *end > second);
            if driver.is_none() {
                if per_unit {
                    driver = ask("driver", aterm_capacity::peak_claim(GIB));
                    width = limit;
                } else {
                    for &(jobs, bytes) in &widths {
                        if let Some(claim) = ask("fixed compile", bytes) {
                            driver = Some(claim);
                            width = jobs;
                            break;
                        }
                    }
                }
                if driver.is_some() {
                    admitted_at = Some(second);
                }
            }
            if driver.is_none() {
                second = if second < 1251 { 1251 } else { 1635 };
                continue;
            }
            // Export retains four of the eighteen tokens through 21m16s; each
            // running slice keeps its implicit token. Fixed admission waits for
            // its complete charge, while per-unit admission uses what is free.
            let available = 18
                - u32::from(second < 1251)
                - u32::from(second < 1635)
                - if second < 1276 { 4 } else { 0 };
            if per_unit {
                width = limit.min(available);
            } else if width > available {
                second = [1251, 1276, 1635]
                    .into_iter()
                    .filter(|at| *at > second)
                    .min()
                    .unwrap();
                continue;
            }
            if jobs.is_empty() && active.is_empty() && !phase_one {
                jobs = std::mem::take(&mut tail);
                phase_one = true;
            }
            let mut index = 0;
            let mut refused = std::collections::BTreeSet::new();
            while index < jobs.len() && active.len() < width as usize {
                let (duration, peak) = jobs[index];
                if refused.contains(&peak) {
                    index += 1;
                    continue;
                }
                let claim = if per_unit {
                    match ask("unit", aterm_capacity::peak_claim(peak)) {
                        Some(claim) => Some(claim),
                        None => {
                            refused.insert(peak);
                            index += 1;
                            continue;
                        }
                    }
                } else {
                    None
                };
                active.push((second + duration, claim));
                jobs.remove(index);
            }
            if second < 1251 {
                widest_before_release = widest_before_release.max(active.len());
            }
            if phase_one && jobs.is_empty() && active.is_empty() {
                return (second + 1355, admitted_at.unwrap(), widest_before_release);
            }
            second = active
                .iter()
                .map(|(end, _)| *end)
                .chain([1251, 1276, 1635].into_iter().filter(|at| *at > second))
                .min()
                .expect("unfinished work has a completion or slice release");
        }
        panic!("model never completed");
    };
    for (native, compat) in [(37, 20), (42, 23)] {
        let fixed = simulate(native, compat, false, CEILING);
        let widening = simulate(native, compat, true, CEILING);
        let narrow = simulate(native, compat, true, 2);
        assert_eq!(fixed.0, 3081); // 51m21s, worse than measured old ordering's 47m34s.
        assert_eq!(fixed.1, 1251);
        assert_eq!(widening.0, 2786); // 46m26s; its heavy unit waits, not all compiler work.
        assert_eq!(widening.1, 0);
        assert!(widening.2 > 0);
        assert!(widening.0 <= 2854);
        assert!(fixed.0 > widening.0);
        assert!(narrow.0 > widening.0);
        eprintln!(
            "m3 {native}+{compat} GiB: fixed={}s, per-unit={}s, fixed-two={}s, old=2854s; initial active width={}",
            fixed.0, widening.0, narrow.0, widening.2
        );
    }
}

#[cfg(unix)]
#[test]
fn cargo_wrapper_admits_named_units_and_passes_through_probes() {
    const CHILD: &str = "ATERM_VERIFY_WRAPPER_TEST_CHILD";
    const LEDGER: &str = "ATERM_VERIFY_WRAPPER_TEST_LEDGER";
    if let Ok(mode) = std::env::var(CHILD) {
        let ledger = aterm_capacity::Ledger::at(std::env::var_os(LEDGER).unwrap());
        let compiler = if mode == "fail" {
            "/usr/bin/false"
        } else {
            "/usr/bin/true"
        };
        let kind = if mode == "missing" { "bin" } else { "lib" };
        let mut args: Vec<OsString> = vec![compiler.into()];
        if mode == "invalid" {
            args.extend(["--crate-type", kind].map(OsString::from));
        } else {
            args.extend(["--crate-name", "fixture", "--crate-type", kind].map(OsString::from));
        }
        if mode == "probe" {
            args.extend(["--print", "file-names"].map(OsString::from));
        }
        if mode == "version" {
            // Cargo's own first call: no crate name, and nothing compiled.
            args = vec![compiler.into(), "-vV".into()];
        }
        assert_eq!(
            entry_on(
                &args,
                &aterm_capacity::Fixed::new(aterm_capacity::Reading::of_gib(128, 76, 18)),
                Some(&ledger),
            ),
            Some(if mode == "fail" { 1 } else { 0 })
        );
        assert!(
            ledger.list().unwrap().iter().all(|row| row.held.is_none()),
            "the compiler claim ends with its child"
        );
        return;
    }
    let dir = aterm_tempfile::tempdir().unwrap();
    let unit = crate::unitgraph::UnitName {
        package: "fixture".into(),
        kind: "fixture lib".into(),
    };
    for mode in [
        "unit", "probe", "version", "missing", "invalid", "record", "fail",
    ] {
        let book = dir.path().join(format!("{mode}.json"));
        let pending = dir.path().join(format!("{mode}.pending.json"));
        if matches!(mode, "unit" | "probe" | "version" | "missing" | "invalid") {
            let mut records = crate::memory::Records::default();
            records.push(
                crate::shape::compiler_unit_key(crate::shape::UNIT_STAGE, "/fixture", &unit),
                2 << 30,
                1,
            );
            records.save(&book).unwrap();
        }
        let mut command = aterm_uds::spawnfd::command(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "compile_width::tests::cargo_wrapper_admits_named_units_and_passes_through_probes",
                "--nocapture",
            ])
            .env(CHILD, mode)
            .env(LEDGER, dir.path().join(format!("{mode}.ledger")))
            .env(WRAPPER, "1")
            .env(UNIT_BOOK, &book)
            .env(UNIT_CHECKOUT, "/fixture")
            .env(
                UNIT_MODE,
                if matches!(mode, "record" | "fail") {
                    "record"
                } else {
                    "claim"
                },
            )
            .env(UNIT_PENDING, &pending)
            .env(UNIT_RUN, "fixture-run")
            .env(UNIT_STAGE, crate::shape::UNIT_STAGE)
            .env(UNIT_UNKNOWN, aterm_capacity::COLD_CLAIM.to_string())
            .env("CARGO_PKG_NAME", "fixture")
            .env("CARGO_BUILD_JOBS", "14");
        let output = aterm_uds::deadline::output_until(
            &mut command,
            std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN,
        )
        .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let log = String::from_utf8_lossy(&output.stderr);
        // NEGATIVE CONTROL: `-vV` read as an unidentified compiler claimed the whole cold
        // envelope for a version string, and was refused on a Mac that can never have it.
        assert_eq!(
            log.contains("compiler width now 1"),
            matches!(mode, "unit" | "missing" | "invalid"),
            "{log}"
        );
        if mode == "unit" {
            assert!(log.contains("3 GiB"), "{log}");
        } else if matches!(mode, "missing" | "invalid") {
            assert!(
                log.contains("16 GiB"),
                "NEGATIVE CONTROL: a missing or malformed unit must claim the full cold envelope, not the old 1 GiB default: {log}"
            );
        }
        let pending = crate::memory::Records::load(&pending);
        let recorded = pending
            .rows()
            .iter()
            .any(|row| valid_unit_row(row, crate::shape::UNIT_STAGE));
        assert_eq!(
            recorded,
            matches!(mode, "unit" | "missing" | "record"),
            "only successful named compilers leave a pending observation ({mode})"
        );
        if mode == "record" {
            assert!(
                !has_units(
                    &crate::memory::Records::load(&book),
                    crate::shape::UNIT_STAGE
                ),
                "NEGATIVE CONTROL: wait4 alone cannot create claimable unit history"
            );
        }
    }
}

/// A whole-workspace release's unmeasured unit asks for the stage's plan (32.8 GiB on
/// 2026-11-12) and the driver's 2 GiB beside it. A 32 GiB Mac can never have that, and the
/// wrapper refused the unit, so the release could not compile its tests there. The unit now
/// runs alone on such a Mac and is admitted once nothing else holds memory; where the claim
/// can fit, it is asked for as before.
#[test]
fn a_unit_claim_this_mac_can_never_hold_runs_alone_instead_of_being_refused() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let unit = |bytes: u64| {
        aterm_capacity::Need::new("verify TestCompile unit compiler unit run fixture lib")
            .mem(bytes)
            .covers()
            .margin(DRIVER_CLAIM)
    };
    let plan = (32 << 30) + (800 << 20);
    let small = aterm_capacity::Reading::of_gib(32, 18, 12);
    assert!(
        aterm_capacity::never(&unit(plan), &small).is_some(),
        "NEGATIVE CONTROL: as it was asked, this Mac can never admit the claim"
    );
    let fitted = unit_need(unit(plan), &small);
    assert!(fitted.alone);
    assert_eq!(aterm_capacity::never(&fitted, &small), None);
    let claim = aterm_capacity::admit::try_admit_with(
        &aterm_capacity::Ledger::at(dir.path().join("small")),
        &aterm_capacity::Fixed::new(small.clone()),
        fitted,
    )
    .unwrap()
    .expect("admitted alone: no other claim holds memory");
    assert!(claim.alone().is_some());
    assert!(
        !unit_need(unit(3 << 30), &small).alone,
        "a claim that fits beside the driver is asked for as it is"
    );
    assert!(
        !unit_need(unit(plan), &aterm_capacity::Reading::of_gib(128, 76, 18)).alone,
        "a Mac that can hold the plan admits it beside the other units"
    );
}

#[cfg(unix)]
#[test]
fn only_a_recorded_test_unit_enables_driver_only_admission_and_the_wrapper() {
    thread_local! { static LEDGER: RefCell<Option<PathBuf>> = const { RefCell::new(None) }; }
    fn admit(
        need: aterm_capacity::Need,
        mode: aterm_capacity::Admit,
    ) -> Result<aterm_capacity::Claim, aterm_capacity::Error> {
        LEDGER.with(|path| {
            aterm_capacity::admit_with(
                &aterm_capacity::Ledger::at(path.borrow().as_ref().unwrap()),
                &aterm_capacity::Fixed::new(aterm_capacity::Reading::of_gib(128, 76, 18)),
                need,
                mode,
                aterm_capacity::Pace::default(),
                &mut |_| {},
            )
        })
    }
    let dir = aterm_tempfile::tempdir().unwrap();
    LEDGER.with(|path| path.replace(Some(dir.path().join("ledger"))));
    let path = dir.path().join("book.json");
    let scope = crate::Scope::crate_only("aterm-gui");
    let mut ctx = crate::Ctx::new(
        dir.path().to_owned(),
        scope.clone(),
        crate::EnvSnapshot::default(),
        dir.path().join("scratch"),
    )
    .with_release(true)
    .with_pool(Some(
        aterm_capacity::Jobserver::new(&dir.path().join("jobs"), 14).unwrap(),
    ))
    .with_memory(crate::memory::Book::at(
        Some(path.clone()),
        Some(128 << 30),
        Some(76 << 30),
    ));
    let mut test_compile = crate::plan::declared(&scope)
        .into_iter()
        .find(|spec| spec.id == crate::plan::StageId::TestCompile)
        .unwrap();
    test_compile.mem = 50 << 30;
    let unit = crate::unitgraph::UnitName {
        package: "aterm-gui".into(),
        kind: "aterm_gui test".into(),
    };

    for jobs in ["1", "3", "14"] {
        ctx.env.cargo_build_jobs = Some(jobs.into());
        let cold_test = claim(&ctx, &test_compile, admit).unwrap().unwrap();
        assert_eq!(
            cold_test.claim.record().need.mem,
            50 << 30,
            "NEGATIVE CONTROL: compiler width {jobs} cannot lower a cold full-stage claim"
        );
        drop(cold_test);
    }
    let test_book = unit_book(&path, crate::plan::StageId::TestCompile);
    let mut valid = crate::memory::Records::default();
    valid.push(
        crate::shape::compiler_unit_key(crate::shape::UNIT_STAGE, "/fixture", &unit),
        9 << 30,
        4,
    );
    valid.save(&test_book).unwrap();
    let known_test = claim(&ctx, &test_compile, admit).unwrap().unwrap();
    assert_eq!(known_test.claim.record().need.mem, DRIVER_CLAIM);
    assert_eq!(known_test.units.as_ref().unwrap().mode, UnitMode::Claim);
}

#[cfg(unix)]
#[test]
fn an_obsolete_tippy_unit_book_cannot_replace_the_whole_stage_claim() {
    thread_local! { static LEDGER: RefCell<Option<PathBuf>> = const { RefCell::new(None) }; }
    fn admit(
        need: aterm_capacity::Need,
        mode: aterm_capacity::Admit,
    ) -> Result<aterm_capacity::Claim, aterm_capacity::Error> {
        LEDGER.with(|path| {
            aterm_capacity::admit_with(
                &aterm_capacity::Ledger::at(path.borrow().as_ref().unwrap()),
                &aterm_capacity::Fixed::new(aterm_capacity::Reading::of_gib(128, 76, 18)),
                need,
                mode,
                aterm_capacity::Pace::default(),
                &mut |_| {},
            )
        })
    }

    let dir = aterm_tempfile::tempdir().unwrap();
    let ledger_path = dir.path().join("ledger");
    LEDGER.with(|path| path.replace(Some(ledger_path.clone())));
    let stage_book = dir.path().join("stage-memory.json");
    let unit = crate::unitgraph::UnitName {
        package: "aterm-gui".into(),
        kind: "aterm_gui lib".into(),
    };
    let mut records = crate::memory::Records::default();
    records.push(
        crate::shape::compiler_unit_key("Tippy unit", "/fixture", &unit),
        9 << 30,
        1,
    );
    records
        .save(&stage_book.with_file_name("tippy-unit-memory.json"))
        .unwrap();
    let scratch = dir.path().join("scratch");
    std::fs::create_dir(&scratch).unwrap();
    let ctx = crate::Ctx::new(
        dir.path().to_owned(),
        crate::Scope::workspace(),
        crate::EnvSnapshot::default(),
        scratch,
    )
    .with_checks(&[crate::Check::Lint])
    .with_release(true)
    .with_pool(Some(
        aterm_capacity::Jobserver::new(&dir.path().join("jobs"), 14).unwrap(),
    ))
    .with_memory(crate::memory::Book::at(
        Some(stage_book),
        Some(128 << 30),
        Some(76 << 30),
    ));
    let spec = crate::plan::plan(&ctx)
        .into_iter()
        .find(|spec| spec.id == crate::plan::StageId::Tippy)
        .unwrap();
    let mut larger = spec.clone();
    larger.mem = 29 << 30;
    let larger_claim = claim(&ctx, &larger, admit).unwrap().unwrap();
    assert_eq!(
        larger_claim.claim.record().need.mem,
        larger.mem,
        "the planned whole-stage need is what the lint claims"
    );
    drop(larger_claim);
    let early = crate::EarlyClaims::for_run(&ctx, std::slice::from_ref(&spec), admit).unwrap();
    {
        let guard = early.claim.lock().unwrap();
        let claim = guard
            .as_ref()
            .and_then(|(_, claim)| claim.as_ref())
            .expect("Tippy is pre-admitted");
        let record = claim.claim.record();
        assert_eq!(record.need.what, format!("verify {}", spec.title));
        assert_eq!(record.need.mem, spec.mem);
        assert!(
            record.need.alone && record.held.is_some_and(|held| held.alone),
            "NEGATIVE CONTROL: a lint never measured here has no figure that bounds it (a 16 GiB \
             cold claim met a 22.3 GiB compile), so it holds the machine's memory alone"
        );
        assert_eq!(record.need.rank, aterm_capacity::Rank::Release);
        assert!(record.need.covers);
        assert_eq!(record.accounting, aterm_capacity::Accounting::Holder);
        assert!(record.held.is_some());
        assert!(
            claim.units.is_none(),
            "NEGATIVE CONTROL: a synthetically seeded old Tippy unit book cannot mint a driver-only claim"
        );
    }

    std::thread::spawn(move || {
        assert!(UNITS.with(|slot| slot.borrow().is_none()));
        let quiet = crate::Quiet::of(&ctx, std::slice::from_ref(&spec), crate::LIVE.admit_quiet);
        let ran = std::cell::Cell::new(false);
        let _ = crate::staged(&ctx, &quiet, &spec, None, crate::LIVE, &early, || {
            let rows = aterm_capacity::Ledger::at(&ledger_path).list().unwrap();
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].accounting, aterm_capacity::Accounting::Holder);
            assert!(rows[0].held.is_some());
            let mut command = aterm_uds::spawnfd::command("unused");
            configure(&mut command);
            assert!(
                !command
                    .get_envs()
                    .any(|(name, _)| name == "RUSTC_WRAPPER" || name == UNIT_MODE)
            );
            ran.set(true);
            let mut report = crate::Report::new("lint");
            report.pass("ran with its exact early claim");
            report
        });
        assert!(ran.get());
        assert!(UNITS.with(|slot| slot.borrow().is_none()));
        assert!(early.take(crate::plan::StageId::Tippy).is_none());
        assert!(
            aterm_capacity::Ledger::at(&ledger_path)
                .list()
                .unwrap()
                .is_empty(),
            "the exact early claim ends with its stage"
        );
    })
    .join()
    .unwrap();
}

fn lint_need(bytes: u64, how: crate::memory::How) -> crate::memory::StageNeed {
    crate::memory::StageNeed { bytes, how }
}

fn learned(peak: u64) -> crate::memory::Learned {
    crate::memory::Learned {
        peak,
        runs: 1,
        wider: None,
    }
}

/// The pooled lint's claim is evidence, not a constant: a peak this machine kept sizes it, on a
/// Mac of any size, and it runs beside other claims; with none kept it asks for the machine's
/// memory alone. NEGATIVE CONTROL: the 28 GiB floor this replaced waited without end on a
/// 32 GiB Mac whose available memory never passed 23 GiB (2026-11-11, twelve hours).
#[test]
fn the_pooled_lint_claims_its_measured_peak_or_the_machine_alone() {
    const GIB: u64 = 1 << 30;
    let small = (Some(32 * GIB), 29 * GIB);
    let measured = lint_claim(
        12 * GIB,
        &lint_need(12 * GIB, crate::memory::How::Learned(learned(10 * GIB))),
        small.0,
        small.1,
    );
    assert_eq!(
        measured,
        LintClaim {
            bytes: 12 * GIB,
            alone: false,
            measured: true
        }
    );
    let elsewhere = lint_claim(
        12 * GIB,
        &lint_need(12 * GIB, crate::memory::How::Elsewhere(learned(10 * GIB))),
        small.0,
        small.1,
    );
    assert!(elsewhere.measured && !elsewhere.alone);
    let cold = lint_claim(
        16 * GIB,
        &lint_need(16 * GIB, crate::memory::How::Cold),
        small.0,
        small.1,
    );
    assert_eq!(
        cold,
        LintClaim {
            bytes: 16 * GIB,
            alone: true,
            measured: false
        }
    );
    let unplanned = lint_claim(
        0,
        &lint_need(0, crate::memory::How::Light),
        small.0,
        small.1,
    );
    assert_eq!(
        (unplanned.bytes, unplanned.alone),
        (aterm_capacity::cold_claim(small.0), true)
    );
    let beyond = lint_claim(
        30 * GIB,
        &lint_need(30 * GIB, crate::memory::How::Learned(learned(25 * GIB))),
        small.0,
        small.1,
    );
    assert!(
        beyond.measured && beyond.alone,
        "a measured peak beyond what can ever be available runs alone"
    );
    let large = lint_claim(
        30 * GIB,
        &lint_need(30 * GIB, crate::memory::How::Learned(learned(25 * GIB))),
        Some(128 * GIB),
        120 * GIB,
    );
    assert!(large.measured && !large.alone);
}

/// The waits end. A measured lint short only of memory no claim holds is admitted alone by the
/// ledger's ALONE rule, and an unmeasured one once no claim holds memory, on a Mac whose
/// available memory stays below either. NEGATIVE CONTROL: the same need with the margin the
/// floor carried is refused that admission and waits.
#[test]
fn the_pooled_lint_is_admitted_on_a_mac_that_never_has_its_claim_free() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let machine = aterm_capacity::Fixed::new(aterm_capacity::Reading::of_gib(32, 18, 12));
    let ask = |name: &str, need: aterm_capacity::Need| {
        aterm_capacity::admit::try_admit_with(
            &aterm_capacity::Ledger::at(dir.path().join(name)),
            &machine,
            need,
        )
        .unwrap()
    };
    let lint = |bytes: u64| {
        aterm_capacity::Need::new("verify tippy lint (--workspace)")
            .rank(aterm_capacity::Rank::Release)
            .mem(bytes)
            .covers()
    };
    let measured = ask("measured", lint(22 << 30)).expect("admitted alone by shortage");
    assert!(measured.alone().is_some());
    let cold = ask("cold", lint(16 << 30).alone()).expect("admitted alone by asking");
    assert!(cold.alone().is_some());
    assert!(
        ask("floor", lint(28 << 30).margin(1)).is_none(),
        "NEGATIVE CONTROL: the margined floor is never admitted here"
    );
}
