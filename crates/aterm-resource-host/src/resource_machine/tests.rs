// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
use super::*;
#[test]
fn actual_counter_deltas_keep_zero_reset_and_width_change_unknown() {
    let zero = CoreTicks::default();
    let quarter = CoreTicks {
        user: 1,
        idle: 3,
        ..zero
    };
    let half = CoreTicks {
        system: 2,
        idle: 2,
        ..zero
    };
    assert_eq!(busy(&[zero, zero], &[quarter, half]), Some(750));
    assert_eq!(busy(&[zero], &[zero]), None);
    assert_eq!(busy(&[zero], &[CoreTicks { idle: 5, ..zero }]), Some(0));
    assert_eq!(busy(&[quarter], &[zero]), None);
    assert_eq!(busy(&[zero], &[quarter, half]), None);
    assert_eq!(busy(&[], &[]), None);
}
#[test]
fn cached_age_never_resets_and_unknown_health_is_not_healthy() {
    let at = Instant::now().checked_sub(Duration::from_secs(2)).unwrap();
    let record = Record {
        at,
        snapshot: Snapshot {
            cpus: 2,
            busy_millicores: None,
            available_memory_bytes: None,
            pressure: pressure(None),
            thermal: thermal(None),
            sample_age: Duration::ZERO,
            window: Duration::from_secs(1),
        },
    };
    let sampler = CachedSampler {
        shared: Arc::new(Shared {
            state: Mutex::new(State {
                stop: false,
                latest: Some(record),
            }),
            wake: Condvar::new(),
        }),
        worker: None,
    };
    let first = sampler.latest().unwrap();
    let second = sampler.latest().unwrap();
    assert!(first.sample_age >= Duration::from_secs(2));
    assert!(second.sample_age >= first.sample_age);
    assert_eq!(first.available_memory_bytes, None);
    assert_eq!(first.pressure, Pressure::Unknown);
    assert_eq!(first.thermal, Thermal::Unknown);

    assert_eq!(
        thermal(Some(Throttle {
            level: ThrottleLevel::Nominal,
            exact: false
        })),
        Thermal::Unknown
    );
}
#[test]
fn native_first_point_has_no_invented_cpu_window() {
    let mut probe = Probe::new();
    let ticks = probe.core_ticks();
    let (snapshot, _) = sample(&mut probe, None, ticks, Instant::now());
    assert_eq!(snapshot.busy_millicores, None);
    assert_eq!(snapshot.window, Duration::ZERO);
    assert_eq!(snapshot.sample_age, Duration::ZERO);
    assert!(snapshot.cpus > 0);
    let sampler = CachedSampler::new().unwrap();
    drop(sampler);
}
