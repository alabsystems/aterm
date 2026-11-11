// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

#[test]
fn concurrent_class_lookups_register_the_framework_without_opening_settings() {
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
    let threads: Vec<_> = (0..8)
        .map(|_| {
            let barrier = std::sync::Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                assert!(!super::class(c"NSObject").is_null());
                // Older supported systems have the framework but not SMAppService.
                assert_eq!(
                    super::class(c"SMAppService"),
                    aterm_objc::class(c"SMAppService")
                );
            })
        })
        .collect();
    for thread in threads {
        thread.join().unwrap();
    }
}
