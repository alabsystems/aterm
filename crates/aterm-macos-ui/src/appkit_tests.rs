// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn concurrent_first_use_reads_real_immutable_exports() {
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
    let threads: Vec<_> = (0..8)
        .map(|_| {
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                aterm_objc::autoreleasepool(|_| {
                    let aqua = appearance_name_aqua();
                    assert!(!aqua.is_null());
                    assert!(!appearance_name_dark_aqua().is_null());
                    assert!(!pasteboard_type_string().is_null());
                    assert!(!event_tracking_run_loop_mode().is_null());
                    assert!(!aterm_objc::class(c"NSApplication").is_null());
                    assert!(version_number().is_finite() && version_number() > 0.0);
                    // SAFETY: AppKit exports this immutable NSString for the process lifetime.
                    let text = unsafe { aterm_objc::ns_string_to_rust(aqua) };
                    assert!(!text.is_empty());
                    (aqua.as_ptr().addr(), text)
                })
            })
        })
        .collect();
    let readings: Vec<_> = threads
        .into_iter()
        .map(|thread| thread.join().unwrap())
        .collect();
    assert!(readings.windows(2).all(|pair| pair[0] == pair[1]));
}
