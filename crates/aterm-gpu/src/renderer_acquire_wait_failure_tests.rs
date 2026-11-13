// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{SurfacePresentFailure, WindowGpu};

#[test]
fn a_refused_acquire_is_measured_and_published_once() {
    let mut window = WindowGpu::new();
    window.begin_surface_present();
    let mut invoked = false;
    let result = window.measure_surface_acquire(|| {
        invoked = true;
        Err::<(), _>(SurfacePresentFailure::Timeout)
    });
    assert!(invoked);
    assert!(matches!(result, Err(SurfacePresentFailure::Timeout)));
    assert_eq!(
        window.take_acquire_wait_sample_ns(),
        Some(window.last_acquire_wait_ns())
    );
    assert_eq!(
        window.take_acquire_wait_sample_ns(),
        None,
        "no duplicate publication"
    );

    window.begin_surface_present();
    assert_eq!(window.take_acquire_wait_sample_ns(), None);
    assert!(matches!(
        window.measure_surface_acquire(|| Ok::<_, SurfacePresentFailure>(7)),
        Ok(7)
    ));
    assert!(window.take_acquire_wait_sample_ns().is_some());
}

#[test]
fn a_new_attempt_discards_an_unconsumed_old_sample() {
    let mut window = WindowGpu::new();
    window.begin_surface_present();
    window.measure_surface_acquire(|| ());
    window.begin_surface_present();
    assert_eq!(window.take_acquire_wait_sample_ns(), None);
}
