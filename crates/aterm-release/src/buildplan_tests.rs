// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

#[test]
fn alternating_pressure_reads_do_not_earn_a_normal_minute() {
    use aterm_capacity::{Ledger, Machine, Need, Reading};
    use std::cell::Cell;
    use std::time::Duration;
    struct Flipping<'a> {
        reads: Cell<usize>,
        elapsed: &'a Cell<Duration>,
        flip_at: Duration,
    }
    impl Machine for Flipping<'_> {
        fn read(&self, _: Option<&std::path::Path>) -> Reading {
            let n = self.reads.get();
            self.reads.set(n + 1);
            let mut reading = Reading::of_gib(96, 35, 28);
            reading.pressure = Some(if self.elapsed.get() < self.flip_at || n % 2 == 0 {
                aterm_sysprobe::MemoryLevel::Normal
            } else {
                aterm_sysprobe::MemoryLevel::Warn
            });
            reading
        }
        fn procs(&self) -> Option<aterm_capacity::Procs> {
            None
        }
    }
    // Exercise both the initial degradation clock and the post-degradation
    // refusal clock. Every other admission sample invalidates continuity.
    for seconds in [0, 60] {
        let dir = aterm_tempfile::tempdir().unwrap();
        let ledger = Ledger::at(dir.path());
        let elapsed = Cell::new(Duration::ZERO);
        let mut lines = Vec::new();
        let machine = Flipping {
            reads: Cell::new(0),
            elapsed: &elapsed,
            flip_at: Duration::from_secs(seconds),
        };
        let why = super::admit_slice_parent_in(
            &ledger,
            &machine,
            Need::new("release slice memory budget")
                .mem_gib(44)
                .margin(1),
            Need::new(crate::slice_units::SERIAL_PARENT)
                .mem(crate::slice_units::serial_parent_bytes(40_u64 << 30))
                .margin(1),
            || elapsed.get(),
            || {
                elapsed.set(elapsed.get() + Duration::from_secs(2));
                elapsed.get() < Duration::from_secs(180)
            },
            |line| lines.push(line.to_owned()),
        )
        .unwrap_err();
        assert!(why.contains("interrupted"), "{why}");
        assert_eq!(
            lines.iter().any(|line| line.contains("degraded after")),
            seconds == 60,
        );
        assert!(ledger.list().unwrap().is_empty());
    }
    // The fixed-normal permanent-shortage test is the negative control: it must
    // still degrade and then hold the exclusive parent instead of disabling the patience
    // clock outright.
}

#[test]
fn an_unclaimed_shortage_below_the_complete_serial_envelope_holds_the_exclusive_parent() {
    use aterm_capacity::{Fixed, Ledger, Need, Polled, Reading, Waiter};
    use std::time::Duration;
    let dir = aterm_tempfile::tempdir().unwrap();
    let ledger = Ledger::at(dir.path());
    let mut reading = Reading::of_gib(96, 35, 28);
    reading.pressure = Some(aterm_sysprobe::MemoryLevel::Normal);
    let machine = Fixed::new(reading);
    let need = Need::new("release slice memory budget")
        .mem_gib(44)
        .margin(1);
    // Fixed readings and no other ledger writer make this today's permanent wait.
    let mut old = Waiter::enter(&ledger, &machine, need.clone()).unwrap();
    for _ in 0..64 {
        old = match old.poll(&machine) {
            Polled::Waiting(next, _) => next,
            other => panic!("old control escaped its permanent shortage: {other:?}"),
        };
    }
    drop(old);
    let elapsed = std::cell::Cell::new(Duration::ZERO);
    let mut lines = Vec::new();
    let why = super::admit_slice_parent_in(
        &ledger,
        &machine,
        need,
        Need::new(crate::slice_units::SERIAL_PARENT)
            .mem(crate::slice_units::serial_parent_bytes(40_u64 << 30))
            .margin(1),
        || elapsed.get(),
        || {
            elapsed.set(elapsed.get() + Duration::from_secs(2));
            assert!(elapsed.get() <= Duration::from_secs(122));
            true
        },
        |line| lines.push(line.to_owned()),
    )
    .unwrap();
    assert_eq!(elapsed.get(), Duration::from_secs(120));
    let (claim, degraded) = why;
    assert!(
        degraded,
        "the slices stay serial under the exclusive parent"
    );
    assert_eq!(claim.record().need.what, crate::slice_units::COLD_PARENT);
    assert!(
        claim.alone().is_some(),
        "no memory claim is admitted beside the exclusive parent"
    );
    assert!(lines.iter().any(|line| line.contains("degraded after 60s")));
    assert!(lines.iter().any(|line| {
        line.contains("cannot be reserved after 60s")
            && line.contains("alone through both serial slices")
    }));
    drop(claim);
    assert!(ledger.list().unwrap().is_empty());
}

#[test]
fn pressure_after_degradation_cancels_the_unclaimed_refusal_clock() {
    use aterm_capacity::{Fixed, Ledger, Need, Reading};
    use std::time::Duration;
    let dir = aterm_tempfile::tempdir().unwrap();
    let ledger = Ledger::at(dir.path());
    let mut reading = Reading::of_gib(96, 35, 28);
    reading.pressure = Some(aterm_sysprobe::MemoryLevel::Normal);
    let machine = Fixed::new(reading.clone());
    let elapsed = std::cell::Cell::new(Duration::ZERO);
    let mut lines = Vec::new();
    let why = super::admit_slice_parent_in(
        &ledger,
        &machine,
        Need::new("release slice memory budget")
            .mem_gib(44)
            .margin(1),
        Need::new(crate::slice_units::SERIAL_PARENT)
            .mem(crate::slice_units::serial_parent_bytes(40_u64 << 30))
            .margin(1),
        || elapsed.get(),
        || {
            elapsed.set(elapsed.get() + Duration::from_secs(2));
            reading.pressure = Some(if (90..150).contains(&elapsed.get().as_secs()) {
                aterm_sysprobe::MemoryLevel::Warn
            } else {
                aterm_sysprobe::MemoryLevel::Normal
            });
            machine.set(reading.clone());
            elapsed.get() < Duration::from_secs(180)
        },
        |line| lines.push(line.to_owned()),
    )
    .unwrap_err();
    assert!(why.contains("interrupted"));
    assert_eq!(elapsed.get(), Duration::from_secs(180));
    assert!(lines.iter().any(|line| line.contains("degraded after 60s")));
    assert!(
        !lines
            .iter()
            .any(|line| line.contains("memory admission refused"))
    );
}

#[test]
fn a_claim_after_degradation_cancels_the_unclaimed_refusal_clock() {
    use aterm_capacity::{Fixed, Ledger, Need, Polled, Reading, Waiter};
    use std::time::Duration;
    let dir = aterm_tempfile::tempdir().unwrap();
    let ledger = Ledger::at(dir.path());
    let mut reading = Reading::of_gib(96, 35, 28);
    reading.pressure = Some(aterm_sysprobe::MemoryLevel::Normal);
    let machine = Fixed::new(reading);
    let elapsed = std::cell::Cell::new(Duration::ZERO);
    let mut holder = None;
    let mut lines = Vec::new();
    let why = super::admit_slice_parent_in(
        &ledger,
        &machine,
        Need::new("release slice memory budget")
            .mem_gib(44)
            .margin(1),
        Need::new(crate::slice_units::SERIAL_PARENT)
            .mem(crate::slice_units::serial_parent_bytes(40_u64 << 30))
            .margin(1),
        || elapsed.get(),
        || {
            elapsed.set(elapsed.get() + Duration::from_secs(2));
            if elapsed.get() == Duration::from_secs(90) {
                let claimant = Waiter::enter(
                    &ledger,
                    &machine,
                    Need::new("competing link")
                        .rank(aterm_capacity::Rank::Release)
                        .mem_gib(8)
                        .margin(1),
                )
                .unwrap();
                holder = Some(match claimant.poll(&machine) {
                    Polled::Admitted(claim) => claim,
                    other => panic!("competing fixture was not admitted: {other:?}"),
                });
            }
            if elapsed.get() == Duration::from_secs(150) {
                drop(holder.take());
            }
            elapsed.get() < Duration::from_secs(180)
        },
        |line| lines.push(line.to_owned()),
    )
    .unwrap_err();
    assert!(why.contains("interrupted"));
    assert_eq!(elapsed.get(), Duration::from_secs(180));
    assert!(lines.iter().any(|line| line.contains("degraded after 60s")));
    assert!(
        !lines
            .iter()
            .any(|line| line.contains("memory admission refused"))
    );
    drop(holder);
}

#[test]
fn packed_slice_admission_rechecks_pressure_and_requires_a_reading() {
    use aterm_capacity::{Fixed, Machine, Reading};
    use aterm_sysprobe::MemoryLevel;
    let mut reading = Reading::of_gib(96, 80, 28);
    let machine = Fixed::new(reading.clone());
    for pressure in [None, Some(MemoryLevel::Warn), Some(MemoryLevel::Critical)] {
        reading.pressure = pressure;
        machine.set(reading.clone());
        assert_eq!(
            super::SliceMemorySample::new(&machine, true)
                .read(None)
                .available,
            Some(0)
        );
    }
    reading.pressure = Some(MemoryLevel::Normal);
    machine.set(reading.clone());
    assert_eq!(
        super::SliceMemorySample::new(&machine, true)
            .read(None)
            .available,
        reading.available
    );
}

#[test]
fn dry_slice_memory_forecast_matches_the_shared_serial_and_refusal_plans() {
    use aterm_capacity::{Fixed, Ledger, Need, Reading};
    use aterm_sysprobe::MemoryLevel;
    const GIB: u64 = 1 << 30;
    let dir = aterm_tempfile::tempdir().unwrap();
    let ledger = Ledger::at(dir.path());
    let mut reading = Reading::of_gib(96, 45, 28);
    reading.pressure = Some(MemoryLevel::Normal);
    let machine = Fixed::new(reading.clone());
    let shared = Need::new("release slice memory budget")
        .rank(aterm_capacity::Rank::Release)
        .mem_gib(44)
        .margin(1);
    let serial = Need::new(crate::slice_units::SERIAL_PARENT)
        .rank(aterm_capacity::Rank::Release)
        .mem(crate::slice_units::serial_parent_bytes(40 * GIB))
        .margin(1);

    let forecast = super::slice_memory_forecast_for_needs_in(
        &ledger,
        &machine,
        shared.clone(),
        serial.clone(),
    )
    .unwrap();
    assert_eq!(forecast.outcome, super::SliceMemoryOutcome::Shared);
    assert_eq!(
        forecast.line,
        "qualify slice memory: shared admits now — 44 GiB parent, 45 GiB available"
    );

    reading.available = Some(43 * GIB);
    machine.set(reading.clone());
    let forecast = super::slice_memory_forecast_for_needs_in(
        &ledger,
        &machine,
        shared.clone(),
        serial.clone(),
    )
    .unwrap();
    assert_eq!(forecast.outcome, super::SliceMemoryOutcome::Serial);
    assert!(
        forecast
            .line
            .contains("degrades to serial and admits — 42 GiB parent, 43 GiB available"),
        "{}",
        forecast.line
    );

    reading.available = Some(397 * GIB / 10);
    machine.set(reading);
    let forecast =
        super::slice_memory_forecast_for_needs_in(&ledger, &machine, shared, serial).unwrap();
    assert_eq!(forecast.outcome, super::SliceMemoryOutcome::Exclusive);
    for words in [
        "shared 44 GiB parent",
        "serial, whose 42 GiB parent is short too",
        "alone through both serial slices",
        "39.7 GiB available",
        "no claim holds it: other work on this Mac",
    ] {
        assert!(forecast.line.contains(words), "{words}: {}", forecast.line);
    }
    assert!(ledger.list().unwrap().is_empty());
}

#[test]
fn dry_slice_memory_forecast_waits_on_and_names_a_claimed_holder() {
    use aterm_capacity::{Fixed, Ledger, Need, Reading};
    use aterm_sysprobe::MemoryLevel;
    const GIB: u64 = 1 << 30;
    let dir = aterm_tempfile::tempdir().unwrap();
    let ledger = Ledger::at(dir.path());
    let mut reading = Reading::of_gib(96, 80, 28);
    reading.pressure = Some(MemoryLevel::Normal);
    let machine = Fixed::new(reading.clone());
    let holder = aterm_capacity::admit::try_admit_with(
        &ledger,
        &machine,
        Need::new("competing link").mem_gib(8).margin(1),
    )
    .unwrap()
    .unwrap();
    reading.available = Some(50 * GIB);
    machine.set(reading);
    let forecast = super::slice_memory_forecast_for_needs_in(
        &ledger,
        &machine,
        Need::new("release slice memory budget")
            .rank(aterm_capacity::Rank::Release)
            .mem_gib(44)
            .margin(1),
        Need::new(crate::slice_units::SERIAL_PARENT)
            .rank(aterm_capacity::Rank::Release)
            .mem(crate::slice_units::serial_parent_bytes(40 * GIB))
            .margin(1),
    )
    .unwrap();
    assert_eq!(forecast.outcome, super::SliceMemoryOutcome::Wait);
    assert!(forecast.line.contains("shared 44 GiB parent would wait"));
    assert!(forecast.line.contains("pid "), "{}", forecast.line);
    assert!(
        forecast.line.contains("competing link"),
        "{}",
        forecast.line
    );
    assert!(
        !forecast.line.contains("degrades to serial"),
        "a claimed shortage never earns the serial clock: {}",
        forecast.line
    );
    drop(holder);
}

#[test]
fn m3_shortage_holds_the_exclusive_parent_without_trimming_the_private_compiler_pool() {
    use aterm_capacity::{Fixed, Ledger, Machine, Need, Polled, Reading, Waiter};
    use std::time::Duration;
    const GIB: u64 = 1 << 30;
    let dir = aterm_tempfile::tempdir().unwrap();
    let ledger = Ledger::at(dir.path());
    let mut reading = Reading::of_gib(96, 40, 28);
    // m3 during 0.198.0's cut, 2026-11-10: 39.7 GiB was available. Neither the
    // 44 GiB two-slice parent nor the complete 42 GiB serial envelope fits.
    reading.available = Some(397 * GIB / 10);
    reading.pressure = Some(aterm_sysprobe::MemoryLevel::Normal);
    let machine = Fixed::new(reading.clone());
    let full = Need::new("release slice memory budget")
        .rank(aterm_capacity::Rank::Release)
        .mem_gib(44)
        .margin(1);
    let serial = Need::new(crate::slice_units::SERIAL_PARENT)
        .rank(aterm_capacity::Rank::Release)
        .mem(crate::slice_units::serial_parent_bytes(40 * GIB))
        .margin(1);
    assert_eq!(serial.mem, 42 * GIB);

    // Today's unchanging admission has no transition out of this state: even
    // a year of injected time would see exactly this same fixed shortfall.
    let mut old = Waiter::enter(&ledger, &machine, full.clone()).unwrap();
    for _ in 0..64 {
        match old.poll(&machine) {
            Polled::Waiting(next, short) => {
                assert!(super::serial_escape(&short, &machine.read(None)));
                old = next;
            }
            other => panic!("negative control admitted: {other:?}"),
        }
    }
    drop(old);
    let elapsed = std::cell::Cell::new(Duration::ZERO);
    let mut lines = Vec::new();
    let why = super::admit_slice_parent_in(
        &ledger,
        &machine,
        full.clone(),
        serial.clone(),
        || elapsed.get(),
        || {
            let next = elapsed.get() + Duration::from_secs(2);
            assert!(
                next <= Duration::from_secs(122),
                "permanent shortage did not converge"
            );
            elapsed.set(next);
            true
        },
        |line| lines.push(line.to_owned()),
    )
    .unwrap();
    assert_eq!(elapsed.get(), Duration::from_secs(120));
    let (claim, degraded) = why;
    assert!(degraded);
    assert_eq!(claim.record().need.what, crate::slice_units::COLD_PARENT);
    assert!(claim.alone().is_some());
    assert!(
        lines.iter().any(|line| line.contains("degraded after 60s")
            && line.contains("compiler claims unchanged"))
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("cannot be reserved after 60s"))
    );
    drop(claim);
    assert!(ledger.list().unwrap().is_empty());

    // NEGATIVE CONTROL: serialization still helps where the full two-driver
    // parent cannot fit but the private pool and one driver can.
    reading.available = Some(43 * GIB);
    machine.set(reading);
    elapsed.set(Duration::ZERO);
    lines.clear();
    let (claim, degraded) = super::admit_slice_parent_in(
        &ledger,
        &machine,
        full,
        serial,
        || elapsed.get(),
        || {
            elapsed.set(elapsed.get() + Duration::from_secs(2));
            true
        },
        |line| lines.push(line.to_owned()),
    )
    .unwrap();
    assert!(degraded);
    assert_eq!(elapsed.get(), Duration::from_secs(60));
    assert_eq!(claim.record().need.mem, 42 * GIB);
}

#[test]
fn pressure_and_claimed_shortages_never_earn_the_serial_escape() {
    use aterm_capacity::{Admit, Fixed, Ledger, Need, Pace, Reading};
    use aterm_sysprobe::MemoryLevel;
    use std::time::Duration;
    for (pressure, claimed) in [
        (MemoryLevel::Warn, false),
        (MemoryLevel::Critical, false),
        (MemoryLevel::Normal, true),
    ] {
        let dir = aterm_tempfile::tempdir().unwrap();
        let ledger = Ledger::at(dir.path());
        let mut reading = Reading::of_gib(96, 80, 28);
        reading.pressure = Some(MemoryLevel::Normal);
        let machine = Fixed::new(reading.clone());
        let holder = claimed.then(|| {
            aterm_capacity::admit_with(
                &ledger,
                &machine,
                Need::new("competing link").mem_gib(8).margin(1),
                Admit::Wait,
                Pace::default(),
                &mut |_| {},
            )
            .unwrap()
        });
        reading.available = Some(409 * (1 << 30) / 10);
        reading.pressure = Some(pressure);
        machine.set(reading);
        let elapsed = std::cell::Cell::new(Duration::ZERO);
        let mut lines = Vec::new();
        let result = super::admit_slice_parent_in(
            &ledger,
            &machine,
            Need::new("release slice memory budget")
                .mem_gib(44)
                .margin(1),
            Need::new(crate::slice_units::SERIAL_PARENT)
                .mem(crate::slice_units::serial_parent_bytes(40_u64 << 30))
                .margin(1),
            || elapsed.get(),
            || {
                elapsed.set(elapsed.get() + Duration::from_secs(2));
                elapsed.get() < Duration::from_secs(180)
            },
            |line| lines.push(line.to_owned()),
        );
        assert!(result.is_err(), "{pressure:?}, claimed={claimed}");
        assert!(!lines.iter().any(|line| line.contains("degraded")));
        assert_eq!(elapsed.get(), Duration::from_secs(180));
        drop(holder);
    }
}

/// A parent its own look names STUCK (no claim's end it can wait for brings what it lacks) earns
/// the serial escape under normal pressure though claims hold memory beside it: builds that fit
/// go first past a stuck parent and are held, and a stream of them must not keep resetting its
/// patience bound (ATERM-KI-1111-25). NEGATIVE CONTROLS: the same holder with a shortage its end
/// may bring, and a stuck parent under pressure, earn none; a parent with no holder and nothing
/// reserved earns it as before.
#[test]
fn a_stuck_parent_earns_the_serial_escape_beside_claims_that_went_first() {
    use aterm_capacity::{Holder, Reading, Short, Shortfall};
    use aterm_sysprobe::MemoryLevel;
    const GIB: u64 = 1 << 30;
    let went_first = Holder {
        pid: 4242,
        what: "test build".to_owned(),
        checkout: String::new(),
        held: true,
        age: std::time::Duration::from_secs(60),
        mem: 0,
        claimed: 3 * GIB,
        alone: false,
        disk: 0,
        quiet: false,
        order: (2, 4242, 0),
        stalled: None,
        credit: Vec::new(),
        overrun: None,
    };
    let short = |stuck: bool, holders: Vec<Holder>| Shortfall {
        shorts: vec![Short::Memory {
            need: 42 * GIB,
            margin: 1,
            available: 32 * GIB,
            reserved: 0,
            passes: None,
            stuck_ahead: 0,
            stuck,
        }],
        holders,
    };
    let mut normal = Reading::of_gib(128, 32, 18);
    normal.pressure = Some(MemoryLevel::Normal);
    let mut warn = normal.clone();
    warn.pressure = Some(MemoryLevel::Warn);
    assert!(super::serial_escape(
        &short(true, vec![went_first.clone()]),
        &normal
    ));
    assert!(!super::serial_escape(
        &short(false, vec![went_first.clone()]),
        &normal
    ));
    assert!(!super::serial_escape(&short(true, vec![went_first]), &warn));
    assert!(super::serial_escape(&short(false, Vec::new()), &normal));
}
#[test]
fn wrapped_compilers_keep_cargos_stdout_in_the_run_before_the_driver_finishes() {
    use std::io::Write as _;
    let dir = aterm_tempfile::tempdir().unwrap();
    let log = dir.path().join("native.log");
    let path = super::retained_messages(Some(&log), false, true).unwrap();
    assert_eq!(path, dir.path().join("native.messages.jsonl"));
    let mut writer = crate::durable::create_fresh(&path).unwrap();
    let message = b"{\"reason\":\"compiler-message\",\"message\":{\"event\":\"release-slice-unit\",\"phase\":\"started\"}}\n";
    writer.write_all(message).unwrap();
    // The run owns the file while the driver is still alive: retaining it needs no rename
    // or completion callback, which a killed build cannot run.
    assert_eq!(std::fs::read(&path).unwrap(), message);
    drop(writer);
    assert!(path.is_file());
    // Negative controls: ordinary unwrapped calls stay temporary; pilots remain retained.
    assert!(super::retained_messages(Some(&log), false, false).is_none());
    assert_eq!(
        super::retained_messages(Some(&log), true, false),
        Some(path)
    );
    assert!(super::retained_messages(None, true, true).is_none());
}
use std::io::Cursor;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _, symlink};

#[test]
fn an_unwrapped_slice_diagnostic_waits_for_isolation() {
    for triple in [super::ARM64, super::X86_64] {
        let dir = aterm_tempfile::tempdir().unwrap();
        let ledger = aterm_capacity::Ledger::at(dir.path().join("ledger"));
        let machine = aterm_capacity::Fixed::new(aterm_capacity::Reading::of_gib(128, 76, 16));
        let need = crate::slice_units::slice_need(triple);
        assert!(
            need.alone,
            "unwrapped diagnostics have no per-compiler admission"
        );
        let other = aterm_capacity::admit::try_admit_with(
            &ledger,
            &machine,
            aterm_capacity::Need::new("tree").mem_gib(1),
        )
        .unwrap()
        .unwrap();
        assert!(
            aterm_capacity::admit::try_admit_with(&ledger, &machine, need.clone())
                .unwrap()
                .is_none()
        );
        drop(other);
        let claim = aterm_capacity::admit::try_admit_with(&ledger, &machine, need)
            .unwrap()
            .unwrap();
        assert!(claim.alone().is_some());
    }
}

/// Under the release's pool the slices make no core claim of their own and draw from that
/// pool; with none handed, `ship build` makes its own; a pool named but unadoptable refuses.
/// Negative control: a `slice_pool` that always calls `own` panics in the first case.
#[test]
fn slices_draw_from_a_handed_pool_without_a_core_claim_of_their_own() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let release = aterm_capacity::Jobserver::new(&dir.path().join("jobs"), 3).unwrap();
    let handed = aterm_capacity::Jobserver::adopt(&release.handed()).unwrap();
    let Ok(chosen) = super::slice_pool(Some(Ok(handed)), || {
        panic!("a handed pool makes no claim of its own")
    }) else {
        panic!("the handed pool is adopted");
    };
    assert!(chosen.own.is_none());
    assert_eq!(chosen.waited, std::time::Duration::ZERO);
    assert_eq!(
        (chosen.pool.slots(), chosen.pool.flags()),
        (3, release.flags())
    );
    let held = chosen.pool.try_acquire().unwrap().expect("a token");
    let rest: Vec<_> = std::iter::from_fn(|| release.try_acquire().unwrap()).collect();
    assert_eq!(
        rest.len(),
        2,
        "the slices drew from the release's own tokens"
    );
    drop(rest);
    drop(held);

    let asked = std::cell::Cell::new(false);
    let own = super::slice_pool(None, || {
        asked.set(true);
        Err("own".into())
    });
    assert!(asked.get() && matches!(own, Err(why) if why == "own"));

    let refused = super::slice_pool(Some(Err(std::io::Error::other("no FIFO"))), || {
        panic!("a pool named but unadoptable is never replaced by a claim of every core")
    });
    assert!(
        matches!(&refused, Err(why) if why.contains(aterm_capacity::POOL_ENV) && why.contains("no FIFO"))
    );
}

/// The cold parent stays borrowed while native completes and then compat runs. NEGATIVE
/// CONTROLS: reversing or overlapping the calls changes the exact event order, while dropping
/// the parent before either call cannot satisfy `serial_slices_while`'s borrow.
#[test]
fn cold_slices_run_native_then_compat_while_the_parent_is_held() {
    let held = std::cell::Cell::new(true);
    struct Parent<'a>(&'a std::cell::Cell<bool>);
    impl Drop for Parent<'_> {
        fn drop(&mut self) {
            self.0.set(false);
        }
    }
    let parent = Parent(&held);
    let events = std::cell::RefCell::new(Vec::new());
    let results = super::serial_slices_while(&parent, [super::ARM64, super::X86_64], |triple| {
        assert!(held.get(), "the cold parent covers this whole driver call");
        events.borrow_mut().push(format!("start {triple}"));
        events.borrow_mut().push(format!("end {triple}"));
        triple
    });
    assert_eq!(results, [super::ARM64, super::X86_64]);
    assert_eq!(
        events.into_inner(),
        [
            format!("start {}", super::ARM64),
            format!("end {}", super::ARM64),
            format!("start {}", super::X86_64),
            format!("end {}", super::X86_64),
        ]
    );
    assert!(held.get());
    drop(parent);
    assert!(!held.get());
}

/// The profile pilot's removable cold target is a scratch claim. Negative control: removing
/// `.scratch()` from `pilot_need` makes this fail while leaving the generic builder green.
#[test]
fn profile_pilot_marks_its_cold_intermediates_as_scratch() {
    let root = std::path::Path::new("/pilot");
    let need = super::pilot_need(crate::profile_pilot::Arm::Fat1, root, 7);
    assert_eq!(need.what, "profile-pilot fat1");
    assert_eq!(need.mem, 50 * (1 << 30));
    assert_eq!(need.cores, 7);
    assert_eq!(need.disk, 30 * (1 << 30));
    assert_eq!(need.path.as_deref(), Some(root));
    assert!(
        need.scratch,
        "the pilot removes its cold target after the take"
    );
}

#[test]
fn compat_compile_keeps_the_main_lanes_cpu_grant() {
    let mut cmd = aterm_uds::spawnfd::command("unused");
    super::compat_budget(&mut cmd, Some("1"), Some("3")).unwrap();
    let env: std::collections::BTreeMap<_, _> = cmd
        .get_envs()
        .map(|(k, v)| {
            (
                k.to_string_lossy().into_owned(),
                v.map(|v| v.to_string_lossy().into_owned()),
            )
        })
        .collect();
    assert_eq!(
        env.get("CARGO_BUILD_JOBS").and_then(Option::as_deref),
        Some("3")
    );
    assert_eq!(
        env.get("BUILD_SLOT_HELD").and_then(Option::as_deref),
        Some("1")
    );
    assert!(super::compat_budget(&mut cmd, Some("1"), Some("0")).is_err());
    assert!(super::compat_budget(&mut cmd, Some("1"), None).is_err());
    let mut ordinary = aterm_uds::spawnfd::command("unused");
    super::compat_budget(&mut ordinary, None, Some("3")).unwrap();
    assert_eq!(ordinary.get_envs().count(), 0);
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn the_build_wait_keeps_status_and_measured_peak_rss() {
    let child = crate::interrupt::GroupChild::spawn(
        aterm_uds::spawnfd::command("/bin/sh").args(["-c", "exit 23"]),
    )
    .unwrap();
    let (status, usage) = super::wait_with_peak(child).unwrap();
    assert_eq!(status.code(), Some(23));
    let event = super::build_cpu_event(
        "build aterm native",
        "aterm",
        None,
        "build",
        status,
        super::BuildUsage {
            peak_bytes: usage.peak_bytes,
            cpu_ns: Some(8_000_000_000),
            starved_ns: Some(2_000_000_000),
            units: Some(super::Units {
                fresh: 3,
                compiled: 2,
                unknown: 1,
                output_bytes: 4096,
            }),
        },
        std::time::Duration::from_secs(3),
        None,
    );
    let value: aterm_json::Value =
        aterm_json::from_str(event.strip_prefix("release-event: ").unwrap()).unwrap();
    assert_eq!(value["call"].as_str(), Some("build aterm native"));
    assert_eq!(value["units"]["fresh"].as_u64(), Some(3));
    assert_eq!(value["units"]["compiled"].as_u64(), Some(2));
    assert_eq!(value["units"]["unknown"].as_u64(), Some(1));
    assert_eq!(value["output_bytes"].as_u64(), Some(4096));
    assert_eq!(value["outcome"].as_str(), Some("FAIL"));
    assert_eq!(value["execution_s"].as_f64(), Some(3.0));
    assert_eq!(value["child_cpu"]["observed_s"].as_f64(), Some(8.0));
    assert_eq!(
        value["child_cpu"]["scope"].as_str(),
        Some("sampled-descendant-processes")
    );
    assert_eq!(value["starved_s"].as_f64(), Some(2.0));
    let unknown = super::build_cpu_event(
        "build aterm native",
        "aterm",
        None,
        "build",
        status,
        super::BuildUsage::default(),
        std::time::Duration::from_secs(3),
        None,
    );
    let value: aterm_json::Value =
        aterm_json::from_str(unknown.strip_prefix("release-event: ").unwrap()).unwrap();
    assert!(value["child_cpu"].is_null());
    assert!(value["units"].is_null() && value["output_bytes"].is_null());
    assert_eq!(
        super::compile_line(
            "build aterm native",
            super::BuildUsage::default(),
            std::time::Duration::from_secs(3)
        ),
        "compile build aterm native: - fresh / - compiled, - CPU-s, - starved-s, 3.0 s wall, peak -, - written"
    );
}

/// A FINISHED SLICE'S LINE NAMES ITS PEAK AS THE METER MEASURED IT: the sampled physical
/// footprint of the build's descendant processes ([`super::SliceBuild::peak_bytes`]), never
/// resident memory; a slice the meter did not read prints no peak.
/// NEGATIVE CONTROL: the line still said "(peak N MiB resident)" after 44ee9608ed0 made the
/// peak a sampled descendant footprint.
#[test]
fn a_finished_slices_line_names_its_peak_a_sampled_descendant_footprint() {
    let line = super::slice_done_line("aarch64-apple-darwin", "12m03s", Some(5 << 30));
    assert_eq!(
        line,
        "==> [aarch64-apple-darwin] done in 12m03s (peak 5120 MiB footprint, sampled descendants)"
    );
    assert!(!line.contains("resident"), "{line}");
    assert_eq!(
        super::slice_done_line("x86_64-apple-darwin", "40s", None),
        "==> [x86_64-apple-darwin] done in 40s"
    );
}

/// Cargo's messages count each unit once by `fresh`, a unit with no `fresh` as neither
/// fresh nor built (verify's reading, `aterm_verify::compile_account`), and the bytes are
/// the files of the units not reported fresh, each file once however many names it has; a
/// fresh unit's files, a file that is gone, a symlink and a line that is not a
/// `compiler-artifact` count nothing. NEGATIVE CONTROL: the symlink names a 5000-byte file,
/// and a unit with no `fresh` read as built would count 6 built.
#[test]
fn the_units_count_fresh_and_built_and_size_only_what_was_built() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let lib = dir.path().join("libbuilt.rlib");
    std::fs::write(&lib, vec![0_u8; 1000]).unwrap();
    let meta = dir.path().join("libbuilt.rmeta");
    std::fs::write(&meta, vec![0_u8; 200]).unwrap();
    let uplifted = dir.path().join("aterm");
    std::fs::write(&uplifted, vec![0_u8; 30]).unwrap();
    let alias = dir.path().join("aterm-0123");
    std::fs::hard_link(&uplifted, &alias).unwrap();
    let fresh = dir.path().join("libfresh.rlib");
    std::fs::write(&fresh, vec![0_u8; 5000]).unwrap();
    let artifact = |files: &[&std::path::Path], fresh: bool| {
        aterm_json::json!({
            "reason": "compiler-artifact",
            "filenames": files.iter().map(|f| f.to_string_lossy().into_owned()).collect::<Vec<_>>(),
            "fresh": fresh,
        })
        .to_string()
    };
    let gone = dir.path().join("gone.rlib");
    let link = dir.path().join("liblink.rlib");
    std::os::unix::fs::symlink(&fresh, &link).unwrap();
    let unsaid = dir.path().join("libunsaid.rlib");
    std::fs::write(&unsaid, vec![0_u8; 7]).unwrap();
    let messages = [
        artifact(&[&lib, &meta], false),
        artifact(&[&uplifted], false),
        artifact(&[&alias, &gone], false),
        artifact(&[&fresh], true),
        artifact(&[&lib], false),
        artifact(&[&link], false),
        aterm_json::json!({
            "reason": "compiler-artifact",
            "filenames": [unsaid.to_string_lossy().into_owned()],
        })
        .to_string(),
        r#"{"reason":"build-script-executed","filenames":["x"]}"#.to_owned(),
        "   Compiling aterm v0.1.0".to_owned(),
    ]
    .join("\n");
    let units = super::units_of(Cursor::new(messages.as_bytes())).unwrap();
    assert_eq!(
        units,
        super::Units {
            fresh: 1,
            compiled: 5,
            unknown: 1,
            output_bytes: 1237,
        }
    );
}

/// A stage whose shipped binary was started and never showed it ran is refused, not
/// cleared, until its hold is spent (a restart here); one it ran from, or one nothing
/// was started from, may go.
#[test]
fn a_stage_whose_shipped_binary_never_showed_it_ran_is_held() {
    use aterm_uds::deadline::{EXEC_STAMP, RAN, stamp_exec};
    let dir = aterm_tempfile::tempdir().unwrap();
    let stage = dir.path().join("stage");
    let ship = super::ship_stage(&stage);
    std::fs::create_dir_all(&ship).unwrap();
    super::refuse_held_stage(&stage).expect("nothing was started from it");
    stamp_exec(&ship);
    let refused = super::refuse_held_stage(&stage).unwrap_err();
    assert!(refused.contains("never showed it ran"), "{refused}");
    std::fs::write(ship.join(RAN), "").unwrap();
    super::refuse_held_stage(&stage).expect("it ran");
    std::fs::remove_file(ship.join(RAN)).unwrap();
    std::fs::write(ship.join(EXEC_STAMP), u64::MAX.to_string()).unwrap();
    super::refuse_held_stage(&stage).expect("a restart spent the hold");
}

use std::ffi::OsString;

use super::{
    BuildOutput, LC_SEGMENT_64, MACH_HEADER_64_LEN, MACH_MAGIC_64, MH_EXECUTE, PrivateSliceDir,
    RELEASE_SYSTEM_PATH, ReleaseEnv, Renderer, SECTION_64_LEN, SEGMENT_COMMAND_64_LEN, SliceArch,
    SliceBuild, cargo_build_args, current_release_uid, parse_thin_macho_update_pin,
    publish_verified_output, release_build_command, release_env, release_tool_path,
    resolve_release_rustup_shim_dir, slice_drives_a_session, turn_verdict,
    validate_app_version_reports, validate_cli_app_version, validate_embedded_update_pin,
    validate_final_slice_records, validate_lipo_architectures, validate_named_cli_app_version,
    validate_slice_update_pin_reports, x86_64_probe_asset_name, x86_64_probe_transcript,
    x86_64_slice_runs,
};

#[test]
fn the_universal_cut_runs_its_x86_64_slice_under_rosetta() {
    use std::os::unix::process::ExitStatusExt as _;
    let output = |code: i32, stdout: &str| std::process::Output {
        status: std::process::ExitStatus::from_raw(code << 8),
        stdout: stdout.as_bytes().to_vec(),
        stderr: b"bad CPU type in executable".to_vec(),
    };
    let bin = std::path::Path::new("/stage/ship/aterm");
    let argv = |command: &aterm_uds::spawnfd::Command| -> Vec<String> {
        std::iter::once(command.get_program())
            .chain(command.get_args())
            .map(|s| s.to_string_lossy().into_owned())
            .collect()
    };

    let scratch = crate::test_scratch_dir("x86diag");
    let home = scratch.path().join("home");

    let err = x86_64_slice_runs(bin, &home, &mut |_| Ok(output(1, ""))).unwrap_err();
    assert_eq!(err, crate::gates::ROSETTA_REFUSAL);

    let mut calls = 0;
    x86_64_slice_runs(bin, &home, &mut |_| {
        calls += 1;
        Ok(output(if calls == 1 { 0 } else { 134 }, ""))
    })
    .unwrap_err();

    let mut seen = Vec::new();
    let mut rendered = Vec::new();
    let mut home_writable = false;
    let report = x86_64_slice_runs(bin, &home, &mut |command| {
        seen.push(argv(command));
        rendered.push(format!("{command:?}"));
        home_writable = std::fs::write(home.join("probe"), b"").is_ok();
        Ok(output(0, "app-version: 0.93.0\n"))
    })
    .unwrap();
    let home_env = format!("HOME={:?}", home.display().to_string());
    assert!(
        rendered[1].starts_with("cd \"/\" && env -i ")
            && rendered[1].contains(&home_env)
            && rendered[1].contains("SHELL=\"/bin/zsh\"")
            && rendered[1].contains("PATH=\"/usr/bin:/bin:/usr/sbin:/sbin\""),
        "{}",
        rendered[1]
    );
    assert!(home_writable, "the probe HOME must exist and be writable");
    assert_eq!(report, "app-version: 0.93.0\n");
    assert_eq!(
        seen[1],
        vec![
            "/usr/bin/arch",
            "-x86_64",
            "/stage/ship/aterm",
            "--diagnose"
        ]
    );
}

#[test]
fn the_universal_cut_drives_a_session_on_its_x86_64_slice() {
    use std::os::unix::process::ExitStatusExt as _;
    let bin = std::path::Path::new("/stage/ship/aterm");
    let wait = std::time::Duration::from_secs(60);
    let turn_out = |code: i32, verdict: &str, rows: &str| std::process::Output {
        status: std::process::ExitStatus::from_raw(code << 8),
        stdout: rows.as_bytes().to_vec(),
        stderr: verdict.as_bytes().to_vec(),
    };
    let settled = "aterm-ctl: turn submitted=1 status=settled seq=4 id=1 dur_ms=188\n";
    let root = crate::test_scratch_dir("x86turn");
    let scratch_for = |name: &str| {
        let dir = root.path().join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    };
    let fake_instance = |command: &mut aterm_uds::spawnfd::Command| {
        let args: Vec<String> = command
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(&args[..2], ["-x86_64", "/stage/ship/aterm"]);
        for flag in ["--headless", "--cpu", "--no-reroute"] {
            assert!(args.iter().any(|a| a == flag), "{flag} missing: {args:?}");
        }
        assert!(
            args.windows(2).any(|w| w == ["--lifeline-fd", "0"]),
            "the translated instance holds the cutter's lifeline: {args:?}"
        );
        let rendered = format!("{command:?}");
        assert!(rendered.contains("env -i "), "{rendered}");
        let sock = &args[args.iter().position(|a| a == "--control-sock").unwrap() + 1];
        aterm_uds::spawnfd::command("/bin/sh")
            .arg("-c")
            .arg(format!("touch '{sock}'; exec sleep 120"))
            .spawn()
    };

    let dir = scratch_for("pass");
    let mut turn_argv = Vec::new();
    slice_drives_a_session(
        bin,
        SliceArch::X86_64,
        Renderer::Cpu,
        &dir,
        &mut |c| fake_instance(c),
        &mut |command| {
            turn_argv = command
                .get_args()
                .map(|a| a.to_string_lossy().into_owned())
                .collect();
            Ok(turn_out(0, settled, "sh$ uname -m\nx86_64\n\n"))
        },
        wait,
    )
    .unwrap();
    assert_eq!(
        turn_argv[..4],
        ["-x86_64", "/stage/ship/aterm", "ctl", "--sock"]
    );
    assert_eq!(
        turn_argv[5..],
        [
            "turn",
            "settle=match:^x86_64$",
            "timeout=30000",
            "uname",
            "-m"
        ]
    );
    let toml = std::fs::read_to_string(dir.join("cfg/aterm/aterm.toml")).unwrap();
    assert!(toml.contains("[update]\nenabled = false") && toml.contains("[packages]"));

    for (verdict, rows, code) in [
        (settled, "sh$ uname -m\narm64\n", 0),
        (
            "aterm-ctl: turn submitted=1 status=timeout seq=7\n",
            "sh$ uname -m\n",
            0,
        ),
        ("aterm-ctl: ERR no session\n", "", 1),
    ] {
        let dir = scratch_for("refuse");
        let err = slice_drives_a_session(
            bin,
            SliceArch::X86_64,
            Renderer::Cpu,
            &dir,
            &mut |c| fake_instance(c),
            &mut |_| Ok(turn_out(code, verdict, rows)),
            wait,
        )
        .unwrap_err();
        assert!(!err.is_empty());
    }
    assert!(turn_verdict(&turn_out(0, settled, "x86_64\n"), SliceArch::X86_64).is_ok());
    assert!(turn_verdict(&turn_out(0, settled, "arm64\n"), SliceArch::Arm64).is_ok());
    turn_verdict(&turn_out(0, settled, "x86_64\n"), SliceArch::Arm64)
        .expect_err("another slice's answer");
    let arm64_instance = |command: &mut aterm_uds::spawnfd::Command| {
        let args: Vec<String> = command
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(&args[..2], ["-arm64", "/stage/ship/aterm"]);
        assert!(!args.iter().any(|a| a == "--cpu"), "{args:?}");
        let sock = &args[args.iter().position(|a| a == "--control-sock").unwrap() + 1];
        aterm_uds::spawnfd::command("/bin/sh")
            .arg("-c")
            .arg(format!("touch '{sock}'; exec sleep 120"))
            .spawn()
    };
    let gpu_drive = |name: &str, image: i32, metrics: &str| {
        let dir = scratch_for(name);
        let mut verbs = Vec::new();
        let driven = slice_drives_a_session(
            bin,
            SliceArch::Arm64,
            Renderer::Gpu,
            &dir,
            &mut |c| arm64_instance(c),
            &mut |command| {
                let argv: Vec<String> = command
                    .get_args()
                    .map(|a| a.to_string_lossy().into_owned())
                    .collect();
                assert_eq!(argv[..4], ["-arm64", "/stage/ship/aterm", "ctl", "--sock"]);
                verbs.push(argv[5..].join(" "));
                Ok(match argv[5].as_str() {
                    "turn" => turn_out(0, settled, "sh$ uname -m\narm64\n"),
                    "image" => turn_out(image, "", "OK 1\n80 24 3 AAAA\n"),
                    _ => turn_out(0, "", metrics),
                })
            },
            wait,
        );
        (driven, verbs)
    };
    let (driven, verbs) = gpu_drive("arm64", 0, "backend=gpu frames=3 fx_under_frames=0\n");
    driven.unwrap();
    assert_eq!(
        verbs,
        [
            "turn settle=match:^arm64$ timeout=30000 uname -m",
            "image --bytes",
            "metrics"
        ]
    );
    let (driven, _) = gpu_drive("arm64-cpu", 0, "backend=cpu frames=3\n");
    driven.unwrap_err();
    let (driven, _) = gpu_drive("arm64-none", 0, "frames=3\n");
    driven.unwrap_err();
    let (driven, verbs) = gpu_drive("arm64-noframe", 1, "backend=gpu\n");
    driven.unwrap_err();
    assert_eq!(verbs.len(), 2, "no metrics read without a frame: {verbs:?}");

    let dir = scratch_for("exit");
    slice_drives_a_session(
        bin,
        SliceArch::X86_64,
        Renderer::Cpu,
        &dir,
        &mut |_| aterm_uds::spawnfd::command("/usr/bin/false").spawn(),
        &mut |_| panic!("no turn without a socket"),
        wait,
    )
    .unwrap_err();

    let dir = scratch_for("silent");
    let err = slice_drives_a_session(
        bin,
        SliceArch::X86_64,
        Renderer::Cpu,
        &dir,
        &mut |_| aterm_uds::spawnfd::command("/bin/sleep").arg("30").spawn(),
        &mut |_| panic!("no turn without a socket"),
        std::time::Duration::ZERO,
    )
    .unwrap_err();
    assert!(err.contains("opened no control socket"), "{err}");
}

const EXPECTED: &str = "529d8b60583fdc58b13afdba7050de6b21c0740b86dd87e5af769a2afb6c30f4";
const WRONG: &str = "b8d47d9179feb56b1cbbe61c000b81f18d1ac152507d8abd320e2a2297890f1f";

#[cfg(target_vendor = "apple")]
#[used]
#[unsafe(link_section = "__DATA,__aterm_upin")]
static NATIVE_MACHO_TEST_RECORD: [u8; 64] =
    *b"529d8b60583fdc58b13afdba7050de6b21c0740b86dd87e5af769a2afb6c30f4";

fn report(pin: &str) -> String {
    format!("aterm diagnostics\nupdate-pin-sha256: {pin}\nrenderer: gpu\n")
}

fn create_test_rustup_shims(directory: &std::path::Path) {
    std::fs::create_dir_all(directory).unwrap();
    std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700)).unwrap();
    for tool in ["cargo", "rustup"] {
        let path = directory.join(tool);
        std::fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o500)).unwrap();
    }
}

/// THE COMPAT SLICE RUNS FROM THE ROW'S RUSTUP'S OWN DIRECTORY: the `rustup` row's
/// answer (its stamped copy, beside its proxies) is the shim directory, canonical, and
/// the build's PATH is it then the system's. No other directory stands in for it.
/// NEGATIVE CONTROL: the same directory without its `cargo` is refused, not replaced.
#[test]
fn rustup_shim_resolver_takes_the_rows_rustups_directory_and_builds_a_closed_path() {
    let scratch = PrivateSliceDir::create().unwrap();
    let bin = scratch.0.join("tooling.noindex/rustup-1.29.2/bin");
    create_test_rustup_shims(&bin);
    let alias = scratch.0.join("alias");
    symlink(scratch.0.join("tooling.noindex"), &alias).unwrap();

    let resolved = resolve_release_rustup_shim_dir(
        &alias.join("rustup-1.29.2/bin/rustup"),
        current_release_uid().unwrap(),
    )
    .unwrap();
    assert_eq!(resolved, bin.canonicalize().unwrap());
    assert_eq!(
        release_tool_path(&resolved).unwrap(),
        std::ffi::OsString::from(format!("{}:{RELEASE_SYSTEM_PATH}", resolved.display()))
    );

    std::fs::remove_file(bin.join("cargo")).unwrap();
    let error =
        resolve_release_rustup_shim_dir(&bin.join("rustup"), current_release_uid().unwrap())
            .unwrap_err();
    assert!(error.contains("inspect release cargo shim"), "{error}");
    let bare = std::path::Path::new("rustup");
    assert!(
        resolve_release_rustup_shim_dir(bare, current_release_uid().unwrap())
            .unwrap_err()
            .contains("names no directory")
    );
}

#[test]
fn rustup_shim_resolver_refuses_an_unsafe_directory() {
    let scratch = PrivateSliceDir::create().unwrap();
    let bin = scratch.0.join("bin");
    create_test_rustup_shims(&bin);
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o770)).unwrap();

    let error =
        resolve_release_rustup_shim_dir(&bin.join("rustup"), current_release_uid().unwrap())
            .unwrap_err();
    assert!(error.contains("group/other-writable"), "{error}");
}

#[test]
fn rustup_shim_resolver_requires_owned_real_executables() {
    let scratch = PrivateSliceDir::create().unwrap();
    let bin = scratch.0.join("bin");
    create_test_rustup_shims(&bin);
    std::fs::remove_file(bin.join("cargo")).unwrap();
    symlink("rustup", bin.join("cargo")).unwrap();

    let error =
        resolve_release_rustup_shim_dir(&bin.join("rustup"), current_release_uid().unwrap())
            .unwrap_err();
    assert!(error.contains("not a real regular file"), "{error}");
}

#[test]
fn anatomy_keeps_all_sealed_configs_in_cargos_global_scope() {
    let config = std::path::Path::new("/take/config.toml");
    let manifest = std::path::Path::new("/take/src/Cargo.toml");
    for target in [None, Some("x86_64-apple-darwin")] {
        let args = super::anatomy_args(
            super::pilot_profile_args(
                cargo_build_args("build", config, config, manifest, "aterm", target, false),
                crate::profile_pilot::Arm::FatGui16,
                target,
            ),
            target,
        );
        let cut = args.iter().position(|arg| arg == "build").unwrap();
        assert_eq!(
            args[..cut].iter().filter(|arg| *arg == "--config").count(),
            4
        );
        assert!(!args[cut..].iter().any(|arg| arg == "--config"));
        assert_eq!(args.first().unwrap() == "--unverified", target.is_none());
        assert!(args.windows(2).any(|pair| pair == ["--bin", "aterm"]));
    }
}

#[test]
fn both_release_arches_use_one_locked_offline_dependency_source() {
    let workspace_config =
        std::path::Path::new("/c-release.noindex/snapshot/src/.cargo/config.toml");
    let source_config = std::path::Path::new("/c-release.noindex/snapshot/.cargo/config.toml");
    let manifest = std::path::Path::new("/c-release.noindex/snapshot/src/Cargo.toml");
    let native = cargo_build_args(
        "build",
        workspace_config,
        source_config,
        manifest,
        "aterm",
        None,
        false,
    );
    let compat = cargo_build_args(
        "build",
        workspace_config,
        source_config,
        manifest,
        "aterm",
        Some("x86_64-apple-darwin"),
        false,
    );
    let text = |args: &[std::ffi::OsString]| {
        args.iter()
            .map(|value| value.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
    };
    let native = text(&native);
    let compat = text(&compat);
    for args in [&native, &compat] {
        for config in [workspace_config, source_config] {
            assert!(
                args.windows(2)
                    .any(|pair| pair == ["--config", config.to_str().unwrap()])
            );
        }
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--manifest-path", manifest.to_str().unwrap()])
        );
        assert!(
            args.iter()
                .any(|arg| arg == "--message-format=json-render-diagnostics")
        );
        assert!(args.contains(&"--locked".to_owned()));
        assert!(args.contains(&"--offline".to_owned()));
        assert!(args.windows(2).any(|pair| pair == ["--bin", "aterm"]));
    }
    assert_eq!(native.first().map(String::as_str), Some("--unverified"));
    assert!(!compat.contains(&"--unverified".to_owned()));
    assert!(
        compat
            .windows(2)
            .any(|pair| pair == ["--target", "x86_64-apple-darwin"])
    );
    let crt = |args: &[String]| args.iter().any(|arg| arg.contains("crt-static"));
    assert!(
        !crt(&native) && !crt(&compat),
        "only a static build links the C library in"
    );
}

/// A slice keeps the features its own package selects: the workspace's feature unification,
/// which verify's test builds take (`aterm_toolchain::build_cache::with_test_graph`),
/// reaches no slice's argv, nor the two configs a slice reads (the sealed one and the
/// checkout's, where only verify's alias carries the words). NEGATIVE CONTROL: the reader
/// finds the words in a test build's argv and a `[resolver]` table in a config.
#[test]
fn a_release_slice_never_resolves_the_workspaces_features() {
    let config = std::path::Path::new("/c-release.noindex/snapshot/src/.cargo/config.toml");
    let manifest = std::path::Path::new("/c-release.noindex/snapshot/src/Cargo.toml");
    let unifies = |args: &[OsString]| {
        args.iter().any(|a| {
            a.to_str()
                .is_some_and(|a| a.contains("feature-unification"))
        })
    };
    for target in [
        None,
        Some("x86_64-apple-darwin"),
        Some("aarch64-unknown-linux-gnu"),
    ] {
        for static_crt in [false, true] {
            for verb in ["build", "check"] {
                let args =
                    cargo_build_args(verb, config, config, manifest, "aterm", target, static_crt);
                assert!(!unifies(&args), "{args:?}");
            }
        }
    }
    let test_build: Vec<OsString> = aterm_toolchain::build_cache::with_test_graph(&[
        "--unverified".to_owned(),
        "test".to_owned(),
    ])
    .into_iter()
    .map(OsString::from)
    .collect();
    assert!(unifies(&test_build), "the reader sees the words");
    // A config line, outside comments and `[alias]` strings, that chooses the unification.
    let chooses = |text: &str| {
        let mut table = String::new();
        text.lines().map(str::trim).any(|line| {
            if line.starts_with('#') {
                return false;
            }
            if let Some(head) = line.strip_prefix('[') {
                table = head.trim_end_matches(']').trim().to_owned();
                return table == "resolver";
            }
            table != "alias"
                && (line.starts_with("resolver") || line.contains("feature-unification"))
        })
    };
    assert!(!chooses(crate::snapshot::SEALED_CONFIG));
    assert!(!chooses(include_str!("../../../.cargo/config.toml")));
    assert!(chooses("[resolver]\nfeature-unification = \"workspace\"\n"));
    assert!(chooses("[unstable]\nfeature-unification = true\n"));
}

#[test]
fn a_static_build_links_the_c_library_into_its_target_alone() {
    let config = std::path::Path::new("/private/take/.cargo/config.toml");
    let manifest = std::path::Path::new("/private/take/Cargo.toml");
    let triple = "aarch64-unknown-linux-gnu";
    let text = |args: Vec<OsString>| -> Vec<String> {
        args.iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    };
    let args = text(cargo_build_args(
        "build",
        config,
        config,
        manifest,
        "aterm-bootstrap-verify",
        Some(triple),
        true,
    ));
    let wanted = aterm_bootstrap_verify::pin::static_build_config(triple);
    assert!(
        args.windows(2)
            .any(|pair| pair == ["--config", wanted.as_str()]),
        "{args:?}"
    );
    assert!(args.windows(2).any(|pair| pair == ["--target", triple]));
    assert!(!args.contains(&"--unverified".to_owned()));
    assert!(!args.contains(&"--bin".to_owned()));
    let native = text(cargo_build_args(
        "build", config, config, manifest, "aterm", None, true,
    ));
    assert!(
        !native.iter().any(|arg| arg.contains("crt-static")),
        "{native:?}"
    );
}

/// A RELEASE SLICE'S CARGO NAMES THIS EXECUTABLE ITS COMPILER WRAPPER AND GAINS NOTHING
/// ELSE: its environment is the build's own plus the wrapper's variables
/// ([`crate::slice_units::Units::env`]), which the wrapper removes again from the compiler it
/// starts. NEGATIVE CONTROL: a build with no units names no wrapper.
#[test]
fn a_slices_cargo_names_this_executable_its_compiler_wrapper_and_gains_nothing_else() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let units = crate::slice_units::Units::of(
        &dir.path().join("units.json"),
        &[super::ARM64],
        1,
        &dir.path().join("budget-ledger"),
    )
    .remove(0);
    let wrapper = std::path::Path::new("/r/aterm-release");
    let root = std::path::Path::new("/private/take/snapshot/src");
    let env = |units| ReleaseEnv {
        path: "/t/bin:/usr/bin".into(),
        home: std::ffi::OsStr::new("/h"),
        tmp: std::path::Path::new("/private/take/tmp"),
        cargo_home: "/private/take/cargo".into(),
        build_number: 4242,
        short_commit: "0123456789ab",
        commit: "0123456789abcdef0123456789abcdef01234567",
        target_dir: "/private/take/target".into(),
        rustup_home: None,
        index_hint: None,
        units,
    };
    let plain = release_env(&env(None));
    let slice = release_env(&env(Some((&units, wrapper, root))));
    let mut gained: Vec<_> = slice.into_iter().filter(|kv| !plain.contains(kv)).collect();
    gained.sort();
    let mut want = units.env(wrapper, root);
    want.sort();
    assert_eq!(gained, want);
    assert!(
        want.iter()
            .any(|(k, v)| *k == "RUSTC_WRAPPER" && v == wrapper.as_os_str())
    );
    assert!(!plain.iter().any(|(k, _)| *k == "RUSTC_WRAPPER"));
}

#[test]
fn the_release_build_enables_no_development_seam() {
    let config = std::path::Path::new("/private/take/.cargo/config.toml");
    let manifest = std::path::Path::new("/private/take/Cargo.toml");
    for target in [None, Some("x86_64-apple-darwin")] {
        let args: Vec<String> =
            cargo_build_args("build", config, config, manifest, "aterm", target, false)
                .iter()
                .map(|a| a.to_string_lossy().into_owned())
                .collect();
        for arg in &args {
            assert!(
                !arg.starts_with("--features")
                    && arg != "-F"
                    && arg != "--all-features"
                    && !arg.contains("dev-seams")
                    && !arg.contains("debug-assertions")
                    && !arg.contains("debug_assertions"),
                "{target:?}: the release build names a feature or debug assertions: {args:?}"
            );
        }
        assert!(
            args.iter().any(|a| a == "--release")
                && !args
                    .iter()
                    .any(|a| a == "--profile" || a.starts_with("--profile=")),
            "{target:?}: the cutter builds the `release` profile the pins below read: {args:?}"
        );
    }
    for (rustup_home, index_hint) in [
        (None, None),
        (None, Some("8..72")),
        (Some(OsString::from("/h/.rustup")), Some("8..72")),
    ] {
        let compat = rustup_home.is_some();
        let e = ReleaseEnv {
            path: "/t/bin:/usr/bin".into(),
            home: std::ffi::OsStr::new("/h"),
            tmp: std::path::Path::new("/private/take/tmp"),
            cargo_home: "/private/take/cargo".into(),
            build_number: 4242,
            short_commit: "0123456789ab",
            commit: "0123456789abcdef0123456789abcdef01234567",
            target_dir: "/private/take/target".into(),
            rustup_home,
            index_hint,
            units: None,
        };
        let cmd = release_build_command(
            std::path::Path::new("/t/bin/targo"),
            cargo_build_args("build", config, config, manifest, "aterm", None, false),
            &e,
        );
        let env: Vec<(String, Option<String>)> = cmd
            .get_envs()
            .map(|(k, v)| {
                (
                    k.to_string_lossy().into_owned(),
                    v.map(|v| v.to_string_lossy().into_owned()),
                )
            })
            .collect();
        let mut want: Vec<(String, Option<String>)> = release_env(&e)
            .into_iter()
            .map(|(k, v)| (k.to_string(), Some(v.to_string_lossy().into_owned())))
            .collect();
        want.sort();
        assert_eq!(
            env, want,
            "the command's environment is release_env's, all of it"
        );
        assert_eq!(cmd.get_current_dir(), Some(std::path::Path::new("/")));
        let get = |k: &str| {
            env.iter()
                .find(|(name, _)| name == k)
                .and_then(|(_, v)| v.clone())
        };
        for steering in [
            "RUSTFLAGS",
            "RUSTDOCFLAGS",
            "CARGO_ENCODED_RUSTFLAGS",
            "CARGO_BUILD_RUSTFLAGS",
            "RUSTC",
            "RUSTC_WRAPPER",
            "RUSTC_BOOTSTRAP",
            "CARGO_FEATURES",
        ] {
            assert_eq!(get(steering), None, "{steering} reaches the release build");
        }
        assert_eq!(
            get(aterm_stamp::input::MODE).as_deref(),
            Some(if cfg!(target_os = "macos") {
                "postlink"
            } else {
                "commit"
            })
        );
        assert_eq!(
            get(aterm_stamp::input::EPOCH).as_deref(),
            Some(if cfg!(target_os = "macos") {
                "0"
            } else {
                "4242"
            })
        );
        let (debug, strip) = if cfg!(target_os = "linux") {
            ("0", "debuginfo")
        } else {
            ("1", "false")
        };
        assert_eq!(get("CARGO_PROFILE_RELEASE_DEBUG").as_deref(), Some(debug));
        assert_eq!(get("CARGO_PROFILE_RELEASE_STRIP").as_deref(), Some(strip));
        assert_eq!(
            get(aterm_stamp::input::RELEASE).as_deref(),
            (!cfg!(target_os = "macos")).then_some("1")
        );
        assert_eq!(get(aterm_capacity::RELEASE_RUN_ENV).as_deref(), Some("1"));
        assert_eq!(
            get(aterm_stamp::input::INDEX_HINT).as_deref(),
            if cfg!(target_os = "macos") {
                None
            } else {
                index_hint
            }
        );
        if cfg!(target_os = "macos") {
            for key in [
                aterm_stamp::input::COMMIT,
                aterm_stamp::input::FULL_COMMIT,
                aterm_stamp::input::DEV_COMMITS,
            ] {
                assert_eq!(
                    get(key),
                    None,
                    "identity pins never reach a postlink compile"
                );
            }
        }
        assert_eq!(
            get("RUSTUP_TOOLCHAIN").as_deref(),
            compat.then_some("stable"),
            "only the compat slice names a stock toolchain"
        );
    }

    let root = aterm_tempfile::manifest_dir().join("..").join("..");
    let mut manifests = vec![root.join("Cargo.toml")];
    for entry in std::fs::read_dir(root.join("crates"))
        .expect("crates/")
        .flatten()
    {
        if !entry
            .file_type()
            .map(|ft| ft.is_dir() || ft.is_symlink())
            .unwrap_or(true)
        {
            continue;
        }
        let path = entry.path().join("Cargo.toml");
        if path.is_file() {
            manifests.push(path);
        }
    }
    assert!(manifests.len() > 20, "found {} manifests", manifests.len());
    let mut forwarders = 0usize;
    for path in &manifests {
        let text = std::fs::read_to_string(path).expect("manifest");
        let doc: aterm_toml::Table =
            aterm_toml::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let shown = path.display();
        if let Some(features) = doc.get("features").and_then(|f| f.as_table()) {
            for (name, list) in features.iter() {
                let names_seam = list.as_array().is_some_and(|items| {
                    items
                        .iter()
                        .any(|i| i.as_str().is_some_and(|v| v.contains("dev-seams")))
                });
                if name == "dev-seams" {
                    forwarders += 1;
                } else {
                    assert!(
                        !names_seam,
                        "{shown}: feature `{name}` enables a development seam"
                    );
                }
            }
        }
        let mut tables: Vec<&aterm_toml::Table> = Vec::new();
        for key in ["dependencies", "build-dependencies", "workspace"] {
            if let Some(t) = doc.get(key).and_then(|t| t.as_table()) {
                tables.push(t);
                if let Some(deps) = t.get("dependencies").and_then(|d| d.as_table()) {
                    tables.push(deps);
                }
            }
        }
        if let Some(targets) = doc.get("target").and_then(|t| t.as_table()) {
            for spec in targets.values().filter_map(|v| v.as_table()) {
                for key in ["dependencies", "build-dependencies"] {
                    if let Some(t) = spec.get(key).and_then(|t| t.as_table()) {
                        tables.push(t);
                    }
                }
            }
        }
        for table in tables {
            for (dep, spec) in table.iter() {
                let enables = spec
                    .as_table()
                    .and_then(|t| t.get("features"))
                    .and_then(|f| f.as_array())
                    .is_some_and(|items| items.iter().any(|i| i.as_str() == Some("dev-seams")));
                assert!(!enables, "{shown}: `{dep}` is depended on with dev-seams");
            }
        }
    }
    assert!(forwarders > 0, "no crate declares the seam feature");

    let workspace: aterm_toml::Table =
        aterm_toml::from_str(&std::fs::read_to_string(root.join("Cargo.toml")).unwrap()).unwrap();
    let release = workspace
        .get("profile")
        .and_then(|p| p.as_table())
        .and_then(|p| p.get("release"))
        .and_then(|r| r.as_table())
        .expect("the workspace declares [profile.release]");
    assert_no_debug_assertions("Cargo.toml [profile.release]", release);
    let config_path = root.join(".cargo/config.toml");
    let config: aterm_toml::Table =
        aterm_toml::from_str(&std::fs::read_to_string(&config_path).unwrap()).unwrap();
    assert_no_debug_assertions(".cargo/config.toml", &config);
}

fn assert_no_debug_assertions(shown: &str, table: &aterm_toml::Table) {
    fn walk(shown: &str, path: &str, value: &aterm_toml::Value) {
        match value {
            aterm_toml::Value::Table(t) => {
                for (key, v) in t.iter() {
                    let here = format!("{path}.{key}");
                    assert!(
                        !(key == "debug-assertions" && v.as_bool() != Some(false)),
                        "{shown}: `{here}` turns debug assertions on — and with them \
                         every development seam in a shipped binary"
                    );
                    walk(shown, &here, v);
                }
            }
            aterm_toml::Value::Array(items) => {
                for item in items {
                    walk(shown, path, item);
                }
            }
            aterm_toml::Value::String(text) => {
                for needle in ["debug-assertions", "debug_assertions", "dev-seams"] {
                    assert!(
                        !text.contains(needle),
                        "{shown}: `{path}` = {text:?} names {needle}"
                    );
                }
            }
            _ => {}
        }
    }
    walk(shown, "", &aterm_toml::Value::Table(table.clone()));
}

#[test]
fn a_verified_take_publishes_its_binary_and_symbols_out_of_the_stage() {
    let scratch = PrivateSliceDir::create().unwrap();
    let stage = scratch.0.join("stage");
    let out = scratch.0.join("dist");
    let dsym = stage.join("aterm.dSYM");
    let zip = stage.join("aterm-0.67.0-dSYM.zip");
    let app = stage.join("aterm.app");
    std::fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
    let binary = app.join("Contents/MacOS/aterm");
    std::fs::write(&binary, "stripped").unwrap();
    std::fs::create_dir_all(&dsym).unwrap();
    std::fs::write(dsym.join("symbol"), "verified").unwrap();
    std::fs::write(&zip, "archive").unwrap();
    let probe = stage.join(x86_64_probe_asset_name("0.67.0"));
    std::fs::write(&probe, x86_64_probe_transcript("0.67.0", "version=0.67.0")).unwrap();

    let published = publish_verified_output(
        BuildOutput {
            app: app.clone(),
            snapshot_digest: "d".repeat(64),
            slices: vec![SliceBuild {
                triple: "aarch64-apple-darwin",
                secs: 1,
                peak_bytes: None,
                cpu_ns: None,
                starved_ns: None,
                units: None,
            }],
            archs: "x86_64 arm64".into(),
            compiler_line: "trust".into(),
            dsym: Some(dsym),
            dsym_zip: Some(zip),
            x86_64_probe: probe,
        },
        &out,
        current_release_uid().unwrap(),
    )
    .unwrap();

    assert_eq!(published.app, out.join("aterm.app"));
    assert_eq!(
        std::fs::read_to_string(out.join("aterm.app/Contents/MacOS/aterm")).unwrap(),
        "stripped"
    );
    assert!(
        !app.exists(),
        "the stage keeps no copy a later take could replace"
    );
    assert_eq!(published.dsym, Some(out.join("aterm.dSYM")));
    assert_eq!(published.dsym_zip, Some(out.join("aterm-0.67.0-dSYM.zip")));
    assert_eq!(
        published.x86_64_probe,
        out.join("aterm-0.67.0-x86_64-probe.txt"),
        "the x86_64 record lands beside the dSYM"
    );
    assert!(
        std::fs::read_to_string(out.join("aterm-0.67.0-x86_64-probe.txt"))
            .unwrap()
            .ends_with("version=0.67.0\n")
    );
    assert_eq!(
        std::fs::read_to_string(out.join("aterm.dSYM/symbol")).unwrap(),
        "verified"
    );
    assert_eq!(
        std::fs::read_to_string(out.join("aterm-0.67.0-dSYM.zip")).unwrap(),
        "archive"
    );
    let out_metadata = std::fs::symlink_metadata(out).unwrap();
    assert_eq!(out_metadata.uid(), current_release_uid().unwrap());
    assert_eq!(out_metadata.permissions().mode() & 0o777, 0o700);
}

#[test]
fn a_verified_take_refuses_a_symlinked_output_directory() {
    let scratch = PrivateSliceDir::create().unwrap();
    let stage = scratch.0.join("stage");
    let outside = scratch.0.join("outside-dist");
    let out = scratch.0.join("dist");
    let dsym = stage.join("aterm.dSYM");
    std::fs::create_dir_all(&dsym).unwrap();
    std::fs::create_dir(&outside).unwrap();
    let sentinel = outside.join("aterm.dSYM");
    std::fs::create_dir(&sentinel).unwrap();
    std::fs::write(sentinel.join("keep"), "outside").unwrap();
    symlink(&outside, &out).unwrap();

    let error = match publish_verified_output(
        BuildOutput {
            app: stage.join("aterm.app"),
            snapshot_digest: "d".repeat(64),
            slices: Vec::new(),
            archs: "x86_64 arm64".into(),
            compiler_line: "trust".into(),
            dsym: Some(dsym.clone()),
            dsym_zip: None,
            x86_64_probe: stage.join("probe.txt"),
        },
        &out,
        current_release_uid().unwrap(),
    ) {
        Ok(_) => panic!("symlinked release output was accepted"),
        Err(error) => error,
    };

    assert!(!error.is_empty());
    assert_eq!(
        std::fs::read_to_string(sentinel.join("keep")).unwrap(),
        "outside"
    );
    assert!(dsym.is_dir());
}

fn write_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn write_u64(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

fn write_name(bytes: &mut [u8], offset: usize, value: &[u8]) {
    assert!(value.len() <= 16);
    bytes[offset..offset + value.len()].copy_from_slice(value);
}

fn thin_macho(sections: &[(&[u8], u64, &[u8])]) -> Vec<u8> {
    let section_count = u32::try_from(sections.len()).unwrap();
    let command_size = SEGMENT_COMMAND_64_LEN + SECTION_64_LEN * u64::from(section_count);
    let commands_end = MACH_HEADER_64_LEN + command_size;
    let data_size: u64 = sections
        .iter()
        .map(|(_, _, data)| u64::try_from(data.len()).unwrap())
        .sum();
    let file_len = commands_end + data_size;
    let mut bytes = vec![0_u8; usize::try_from(file_len).unwrap()];

    write_u32(&mut bytes, 0, MACH_MAGIC_64);
    write_u32(&mut bytes, 4, super::CPU_TYPE_ARM64);
    write_u32(&mut bytes, 12, MH_EXECUTE);
    write_u32(&mut bytes, 16, 1);
    write_u32(&mut bytes, 20, u32::try_from(command_size).unwrap());

    let segment = MACH_HEADER_64_LEN as usize;
    write_u32(&mut bytes, segment, LC_SEGMENT_64);
    write_u32(
        &mut bytes,
        segment + 4,
        u32::try_from(command_size).unwrap(),
    );
    write_name(&mut bytes, segment + 8, b"__DATA");
    write_u64(&mut bytes, segment + 40, commands_end);
    write_u64(&mut bytes, segment + 48, data_size);
    write_u32(&mut bytes, segment + 64, section_count);

    let mut data_offset = commands_end;
    for (index, (name, declared_size, data)) in sections.iter().enumerate() {
        let section = segment + SEGMENT_COMMAND_64_LEN as usize + index * SECTION_64_LEN as usize;
        write_name(&mut bytes, section, name);
        write_name(&mut bytes, section + 16, b"__DATA");
        write_u64(&mut bytes, section + 40, *declared_size);
        write_u32(
            &mut bytes,
            section + 48,
            u32::try_from(data_offset).unwrap(),
        );
        let end = data_offset + u64::try_from(data.len()).unwrap();
        bytes[usize::try_from(data_offset).unwrap()..usize::try_from(end).unwrap()]
            .copy_from_slice(data);
        data_offset = end;
    }
    bytes
}

fn parse_fixture(bytes: &[u8]) -> Result<String, String> {
    parse_thin_macho_update_pin(&mut Cursor::new(bytes), u64::try_from(bytes.len()).unwrap())
}

#[test]
fn native_and_final_runtime_reports_must_match_exact_authority_pin() {
    let arm = report(EXPECTED);
    let x86 = report(EXPECTED);
    assert!(
        validate_slice_update_pin_reports(EXPECTED, &[("arm64", &arm), ("x86_64", &x86)]).is_ok()
    );

    let empty = report("empty");
    assert!(validate_slice_update_pin_reports(EXPECTED, &[("arm64", &empty)]).is_err());
    let wrong = report(WRONG);
    assert!(validate_slice_update_pin_reports(EXPECTED, &[("arm64", &wrong)]).is_err());
    assert!(
        validate_slice_update_pin_reports(EXPECTED, &[("arm64", &arm), ("x86_64", &wrong)])
            .is_err()
    );
    assert!(validate_slice_update_pin_reports(EXPECTED, &[("arm64", "")]).is_err());
    let duplicate = format!("{}update-pin-sha256: {EXPECTED}\n", report(EXPECTED));
    assert!(validate_slice_update_pin_reports(EXPECTED, &[("arm64", &duplicate)]).is_err());
}

#[test]
fn app_version_reports_and_cli_identities_must_match_claim_exactly() {
    let report = "aterm diagnostics\nversion:   0.2.0 (abc123, built now)\nrenderer: gpu\n";
    assert!(validate_app_version_reports("0.2.0", &[("universal", report)]).is_ok());
    assert!(validate_app_version_reports("0.3.0", &[("universal", report)]).is_err());
    assert!(validate_app_version_reports("0.2.1", &[("universal", report)]).is_err());
    assert!(validate_app_version_reports("0.2.0", &[("universal", "")]).is_err());
    let duplicate = format!("{report}version:   0.2.0 (abc123, built now)\n");
    assert!(validate_app_version_reports("0.2.0", &[("universal", &duplicate)]).is_err());

    assert!(validate_cli_app_version("0.2.0", b"aterm 0.2.0\n").is_ok());
    assert!(validate_cli_app_version("0.2.0", b"aterm 0.2.1\n").is_err());
    assert!(validate_named_cli_app_version("aterm-gui", "0.2.0", b"aterm-gui 0.2.0\n").is_ok());
    assert!(validate_named_cli_app_version("aterm-ctl", "0.2.0", b"aterm-ctl 0.2.0\n").is_ok());
    assert!(validate_named_cli_app_version("aterm-ctl", "0.2.0", b"aterm-gui 0.2.0\n").is_err());
    assert!(
        validate_cli_app_version(
            "0.2.0",
            b"aterm 0.2.0\nrunning: /Applications/aterm.app\nanother copy: \
              /Users//ana/Applications/aterm.app (0.1.0) \xe2\x80\x94 the updater leaves \
              it alone\n"
        )
        .is_ok()
    );
    assert!(validate_cli_app_version("0.2.0", b"aterm 0.2.0\nrunning: /x/aterm\n").is_ok());
    let origin = aterm_types::identity::ORIGIN_LINE;
    assert!(
        validate_cli_app_version(
            "0.2.0",
            format!("aterm 0.2.0\n{origin}\nrunning: /x/aterm\n").as_bytes()
        )
        .is_ok()
    );
    assert!(
        validate_cli_app_version("0.2.0", format!("aterm 0.2.0\n{origin}\n").as_bytes()).is_ok()
    );
    assert!(
        validate_cli_app_version("0.2.0", b"aterm 0.2.0\nby somebody else\n").is_err(),
        "only the shared origin line, verbatim"
    );
    assert!(
        validate_cli_app_version(
            "0.2.0",
            format!("aterm 0.2.0\nrunning: /x/aterm\n{origin}\n").as_bytes()
        )
        .is_err(),
        "the origin line sits under the identity line, not after the copies"
    );
    assert!(
        validate_cli_app_version("0.2.1", b"aterm 0.2.0\nrunning: /x/aterm\n").is_err(),
        "the identity line is still exact"
    );
    assert!(
        validate_cli_app_version("0.2.0", b"aterm 0.2.0\nwarning: stale slice\n").is_err(),
        "anything but the which-copy lines still fails"
    );
    assert!(
        validate_cli_app_version("0.2.0", b"aterm 0.2.0\n\nrunning: /x/aterm\n").is_err(),
        "a blank line is not a which-copy line"
    );
    assert!(
        validate_cli_app_version("0.2.0", b"aterm 0.2.0\nrunning: /x/aterm").is_err(),
        "the report is newline-terminated"
    );
    assert!(
        validate_cli_app_version("0.2.0", b"running: /x/aterm\naterm 0.2.0\n").is_err(),
        "the identity line comes first"
    );
}

#[test]
fn thin_macho_parser_accepts_one_exact_dedicated_record() {
    let bytes = thin_macho(&[(b"__aterm_upin", 64, EXPECTED.as_bytes())]);
    let observed = parse_fixture(&bytes).expect("valid dedicated section");
    assert_eq!(observed, EXPECTED);
    validate_embedded_update_pin(EXPECTED, &observed, "arm64").unwrap();
}

#[cfg(target_vendor = "apple")]
#[test]
fn parser_reads_the_real_native_test_macho_section() {
    let executable = std::env::current_exe().expect("current test executable");
    let observed = super::read_thin_macho_update_pin(&executable)
        .expect("parse dedicated section from the real native Mach-O test binary");
    assert_eq!(observed, EXPECTED);
}

#[test]
fn thin_macho_parser_rejects_missing_duplicate_and_wrong_length_records() {
    let missing = thin_macho(&[]);
    assert!(parse_fixture(&missing).unwrap_err().contains("missing"));

    let duplicate = thin_macho(&[
        (b"__aterm_upin", 64, EXPECTED.as_bytes()),
        (b"__aterm_upin", 64, EXPECTED.as_bytes()),
    ]);
    assert!(parse_fixture(&duplicate).unwrap_err().contains("duplicate"));

    for declared_size in [63, 65] {
        let wrong_length = thin_macho(&[(b"__aterm_upin", declared_size, EXPECTED.as_bytes())]);
        assert!(parse_fixture(&wrong_length).unwrap_err().contains("length"));
    }
}

#[test]
fn thin_macho_parser_rejects_wrong_segment_bounds_and_noncanonical_bytes() {
    let mut wrong_segment = thin_macho(&[(b"__aterm_upin", 64, EXPECTED.as_bytes())]);
    let segment = MACH_HEADER_64_LEN as usize;
    wrong_segment[segment + 8..segment + 24].fill(0);
    write_name(&mut wrong_segment, segment + 8, b"__WRONG");
    assert!(
        parse_fixture(&wrong_segment)
            .unwrap_err()
            .contains("__DATA")
    );

    let mut outside = thin_macho(&[(b"__aterm_upin", 64, EXPECTED.as_bytes())]);
    let section = segment + SEGMENT_COMMAND_64_LEN as usize;
    write_u32(&mut outside, section + 48, u32::MAX);
    assert!(parse_fixture(&outside).unwrap_err().contains("outside"));

    let uppercase = EXPECTED.to_ascii_uppercase();
    let noncanonical = thin_macho(&[(b"__aterm_upin", 64, uppercase.as_bytes())]);
    assert!(
        parse_fixture(&noncanonical)
            .unwrap_err()
            .contains("canonical")
    );

    let mut zero_fill = thin_macho(&[(b"__aterm_upin", 64, EXPECTED.as_bytes())]);
    write_u32(&mut zero_fill, section + 64, 1);
    assert!(parse_fixture(&zero_fill).unwrap_err().contains("S_REGULAR"));
}

#[test]
fn raw_fingerprint_substrings_are_never_authority() {
    let mut missing_with_decoy = thin_macho(&[]);
    missing_with_decoy.extend_from_slice(EXPECTED.as_bytes());
    assert!(parse_fixture(&missing_with_decoy).is_err());

    let mut wrong_record_with_decoy = thin_macho(&[(b"__aterm_upin", 64, WRONG.as_bytes())]);
    wrong_record_with_decoy.extend_from_slice(EXPECTED.as_bytes());
    let observed = parse_fixture(&wrong_record_with_decoy).unwrap();
    assert_eq!(observed, WRONG);
    assert!(validate_embedded_update_pin(EXPECTED, &observed, "x86_64").is_err());
}

#[test]
fn malformed_thin_macho_metadata_fails_closed() {
    let valid = thin_macho(&[(b"__aterm_upin", 64, EXPECTED.as_bytes())]);

    let mut fat_magic = valid.clone();
    write_u32(&mut fat_magic, 0, 0xcafe_babe);
    assert!(parse_fixture(&fat_magic).is_err());

    let mut truncated_commands = valid.clone();
    write_u32(&mut truncated_commands, 20, u32::MAX);
    assert!(parse_fixture(&truncated_commands).is_err());

    let mut impossible_count = valid;
    write_u32(&mut impossible_count, 16, u32::MAX);
    assert!(parse_fixture(&impossible_count).is_err());

    let mut wrong_segment_command = thin_macho(&[(b"__aterm_upin", 64, EXPECTED.as_bytes())]);
    write_u32(
        &mut wrong_segment_command,
        MACH_HEADER_64_LEN as usize,
        super::LC_SEGMENT,
    );
    assert!(
        parse_fixture(&wrong_segment_command)
            .unwrap_err()
            .contains("32-bit")
    );
}

#[test]
fn final_shipped_slice_proof_requires_exact_architecture_coverage() {
    let required = ["arm64", "x86_64"];
    assert!(
        validate_final_slice_records(
            EXPECTED,
            &required,
            &[("arm64", EXPECTED), ("x86_64", EXPECTED)],
        )
        .is_ok()
    );
    assert!(validate_final_slice_records(EXPECTED, &required, &[("arm64", EXPECTED)]).is_err());
    assert!(
        validate_final_slice_records(
            EXPECTED,
            &required,
            &[("arm64", EXPECTED), ("arm64", EXPECTED)],
        )
        .is_err()
    );
    assert!(
        validate_final_slice_records(
            EXPECTED,
            &required,
            &[("arm64", EXPECTED), ("x86_64", WRONG)],
        )
        .is_err()
    );
    assert!(
        validate_final_slice_records(
            EXPECTED,
            &required,
            &[("arm64", EXPECTED), ("ppc64", EXPECTED)],
        )
        .is_err()
    );

    assert!(validate_lipo_architectures("x86_64 arm64").is_ok());
    for invalid in ["arm64", "x86_64", "arm64 arm64", "arm64 x86_64 ppc64"] {
        assert!(validate_lipo_architectures(invalid).is_err(), "{invalid}");
    }
}

/// A measured parent larger than the machine is not waited for. This Mac (32 GiB) measured its
/// native link at 33.7 GiB under the exclusive parent on 2026-11-11, so its retained budget
/// exceeds its memory: the next build holds the exclusive parent at once, with no patience
/// clock. NEGATIVE CONTROL: where only the two-slice parent is beyond the machine, the build
/// starts on the serial parent instead, and a parent the machine can hold is still asked first.
#[test]
fn a_measured_parent_beyond_the_machine_holds_the_exclusive_parent_at_once() {
    use aterm_capacity::{Fixed, Ledger, Need, Reading};
    use std::time::Duration;
    const GIB: u64 = 1 << 30;
    let admit = |physical: u64, available: u64, shared: u64, budget: u64| {
        let dir = aterm_tempfile::tempdir().unwrap();
        let ledger = Ledger::at(dir.path());
        let mut reading = Reading::of_gib(physical, available, 12);
        reading.pressure = Some(aterm_sysprobe::MemoryLevel::Normal);
        let machine = Fixed::new(reading);
        let elapsed = std::cell::Cell::new(Duration::ZERO);
        let mut lines = Vec::new();
        let (claim, degraded) = super::admit_slice_parent_in(
            &ledger,
            &machine,
            Need::new("release slice memory budget")
                .rank(aterm_capacity::Rank::Release)
                .mem_gib(shared)
                .margin(1),
            Need::new(crate::slice_units::SERIAL_PARENT)
                .rank(aterm_capacity::Rank::Release)
                .mem(crate::slice_units::serial_parent_bytes(budget * GIB))
                .margin(1),
            || elapsed.get(),
            || {
                elapsed.set(elapsed.get() + Duration::from_secs(2));
                assert!(elapsed.get() <= Duration::from_secs(4), "it waited");
                true
            },
            |line| lines.push(line.to_owned()),
        )
        .unwrap();
        let record = claim.record().clone();
        (
            record,
            claim.alone().is_some(),
            degraded,
            elapsed.get(),
            lines,
        )
    };

    let (record, alone, degraded, waited, lines) = admit(32, 20, 45, 41);
    assert_eq!(record.need.what, crate::slice_units::COLD_PARENT);
    assert!(alone && degraded);
    assert_eq!(waited, Duration::ZERO);
    assert!(
        lines
            .iter()
            .any(|line| line.contains("cannot be reserved on this machine")),
        "{lines:?}"
    );

    let (record, alone, degraded, waited, _) = admit(48, 46, 50, 40);
    assert_eq!(record.need.what, crate::slice_units::SERIAL_PARENT);
    assert!(!alone && degraded);
    assert_eq!(waited, Duration::ZERO);

    let (record, alone, degraded, _, _) = admit(96, 80, 44, 40);
    assert_eq!(record.need.what, "release slice memory budget");
    assert!(!alone && !degraded);
}
