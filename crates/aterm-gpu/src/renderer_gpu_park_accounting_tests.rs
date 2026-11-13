// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::WindowGpu;

#[test]
fn a_gpu_park_accumulates_per_attempt_and_publishes_exactly_once() {
    let mut window = WindowGpu::new();
    window.begin_surface_present();
    assert_eq!(
        window.take_gpu_park_sample_ns(),
        None,
        "an attempt that never parked must publish no sample"
    );

    window.begin_surface_present();
    window.add_gpu_park(3_000_000);
    window.add_gpu_park(7_000_000);
    assert_eq!(window.last_gpu_park_ns(), 10_000_000);
    assert_eq!(window.take_gpu_park_sample_ns(), Some(10_000_000));
    assert_eq!(
        window.take_gpu_park_sample_ns(),
        None,
        "no duplicate publication"
    );

    window.begin_surface_present();
    assert_eq!(window.take_gpu_park_sample_ns(), None);
    window.add_gpu_park(250_000);
    assert_eq!(window.take_gpu_park_sample_ns(), Some(250_000));
}
