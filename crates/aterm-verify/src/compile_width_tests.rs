// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::memory::StageKey;

#[cfg(unix)]
#[test]
fn the_real_lint_stage_installs_pool_flags_without_a_unit_memory_claim() {
    use crate::{Check, Ctx, EnvSnapshot, Scope};
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut ctx = Ctx::new(
        dir.path().to_owned(),
        Scope::crate_only("aterm-verify"),
        EnvSnapshot::default(),
        dir.path().join("scratch"),
    )
    .with_checks(&[Check::Lint]);
    ctx.pool = Some(aterm_capacity::Jobserver::new(&dir.path().join("jobs"), 4).unwrap());
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
    // No memory claim, yet the observation must belong to the pooled width.
    spec.mem = 0;
    let events = dir.path().join("admission.events");
    ctx.events = Some(crate::event_log::EventLog::open(&events).unwrap());
    let quiet = crate::Quiet::of(&ctx, std::slice::from_ref(&spec), crate::LIVE.admit_quiet);
    let observed = std::cell::Cell::new(false);
    let _ =
        crate::staged(
            &ctx,
            &quiet,
            &spec,
            None,
            crate::LIVE,
            || {},
            || {
                let recorded = std::fs::read_to_string(&events).unwrap();
                assert!(
                    recorded
                        .lines()
                        .filter_map(|line| line.strip_prefix("verify-event: "))
                        .filter_map(|line| aterm_json::from_str::<aterm_json::Value>(line).ok())
                        .any(|event| event["event"].as_str() == Some("stage-admitted")
                            && event["stage_id"].as_str() == Some("Tippy")),
                    "resource admission must be visible before lint work starts"
                );
                assert_eq!(current(), Some(3));
                let pool = ctx.pool.as_ref().unwrap();
                let mut command = aterm_uds::spawnfd::command("unused");
                configure(&mut command);
                assert!(command.get_envs().any(
                    |(k, v)| k == "MAKEFLAGS" && v == Some(std::ffi::OsStr::new(&pool.flags()))
                ));
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
            },
        );
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
    for stage in [
        StageId::Tippy,
        StageId::Doctests,
        StageId::SpecCheck,
        StageId::TestCompile,
    ] {
        let jobs = stage_jobs(stage, Some(pool.slots()), Some(3));
        assert_eq!(jobs, Some(3));
        run(Some(&pool), jobs, &|| false, || {
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
    // Negative controls: pure work has no compiler token; unpooled calls do not invent a pool.
    assert_eq!(stage_jobs(StageId::Formatting, Some(4), None), None);
    assert_eq!(stage_jobs(StageId::Tippy, None, Some(4)), None);
    assert_eq!(stage_jobs(StageId::TestCompile, Some(4), Some(2)), Some(2));
}

#[test]
fn unit_envelopes_match_the_package_and_compiler_kind() {
    let mut records = crate::memory::Records::default();
    let unit = crate::unitgraph::UnitName {
        package: "aterm-gui".into(),
        kind: "aterm_gui test".into(),
    };
    let key = crate::shape::unit_key("/old", &unit);
    records.push(key, 196 * (1 << 30) / 10, 1);
    assert_eq!(unit_peak(&records, &unit), 196 * (1 << 30) / 10);
    let lib = crate::unitgraph::UnitName {
        kind: "aterm_gui lib".into(),
        ..unit.clone()
    };
    assert_eq!(unit_peak(&records, &lib), crate::memory::UNIT_FLOOR);
    let other = crate::unitgraph::UnitName {
        package: "other".into(),
        ..unit
    };
    assert_eq!(unit_peak(&records, &other), crate::memory::UNIT_FLOOR);
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
    run(Some(&pool), Some(3), &|| false, || {
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
        let mut args: Vec<OsString> = vec!["/usr/bin/true".into()];
        args.extend(["--crate-name", "fixture", "--crate-type", "lib"].map(OsString::from));
        if mode == "probe" {
            args.extend(["--print", "file-names"].map(OsString::from));
        }
        assert_eq!(
            entry_on(
                &args,
                &aterm_capacity::Fixed::new(aterm_capacity::Reading::of_gib(128, 76, 18)),
                Some(&ledger),
            ),
            Some(if mode == "missing" { 2 } else { 0 })
        );
        assert!(
            ledger
                .list()
                .unwrap()
                .iter()
                .all(|r| !r.need.what.starts_with(UNIT_LABEL))
        );
        return;
    }
    let dir = aterm_tempfile::tempdir().unwrap();
    let book = dir.path().join("units.json");
    let mut records = crate::memory::Records::default();
    let unit = crate::unitgraph::UnitName {
        package: "fixture".into(),
        kind: "fixture lib".into(),
    };
    records.push(crate::shape::unit_key("/fixture", &unit), 2 << 30, 1);
    records.save(&book).unwrap();
    for mode in ["unit", "probe", "missing"] {
        let mut command = aterm_uds::spawnfd::command(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "compile_width::tests::cargo_wrapper_admits_named_units_and_passes_through_probes",
                "--nocapture",
            ])
            .env(CHILD, mode)
            .env(LEDGER, dir.path().join("ledger"))
            .env(WRAPPER, "1")
            .env(
                UNIT_BOOK,
                if mode == "unit" {
                    book.clone()
                } else {
                    dir.path().join("missing")
                },
            )
            .env(UNIT_RUN, "fixture-run")
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
        assert_eq!(
            log.contains("compiler width now 1"),
            mode == "unit",
            "{log}"
        );
        if mode == "unit" {
            assert!(log.contains("3 GiB"), "{log}");
        }
    }
}

#[cfg(unix)]
#[test]
fn only_a_recorded_unit_enables_driver_only_admission_and_the_wrapper() {
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
    let mut spec = crate::plan::declared(&scope)
        .into_iter()
        .find(|spec| spec.id == crate::plan::StageId::TestCompile)
        .unwrap();
    spec.mem = 50 << 30;
    let ctx = crate::Ctx::new(
        dir.path().to_owned(),
        scope,
        crate::EnvSnapshot::default(),
        dir.path().join("scratch"),
    )
    .with_pool(Some(
        aterm_capacity::Jobserver::new(&dir.path().join("jobs"), 14).unwrap(),
    ))
    .with_memory(crate::memory::Book::at(
        Some(path.clone()),
        Some(128 << 30),
        Some(76 << 30),
    ));
    for measured in [false, true] {
        if measured {
            let mut records = crate::memory::Records::default();
            records.push(
                crate::shape::unit_key(
                    "/fixture",
                    &crate::unitgraph::UnitName {
                        package: "aterm-gui".into(),
                        kind: "aterm_gui test".into(),
                    },
                ),
                196 * (1 << 30) / 10,
                1,
            );
            records.save(&path).unwrap();
        }
        let claim = claim(&ctx, &spec, admit).unwrap().unwrap();
        assert_eq!(
            claim.claim.record().need.mem,
            if measured { 2 << 30 } else { 50 << 30 }
        );
        let jobs = stage_jobs(
            spec.id,
            ctx.pool.as_ref().map(aterm_capacity::Jobserver::slots),
            ctx.build_jobs_ceiling(),
        );
        run(ctx.pool.as_ref(), jobs, &|| false, || {
            let mut command = aterm_uds::spawnfd::command("unused");
            configure(&mut command);
            assert_eq!(
                command
                    .get_envs()
                    .any(|(key, value)| key == "RUSTC_WRAPPER" && value.is_some()),
                measured
            );
        })
        .unwrap();
        assert!(UNITS.with(|slot| slot.borrow().is_none()));
    }
}
