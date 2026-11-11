// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

#[test]
fn concurrent_first_use_resolves_one_complete_real_table_without_a_dns_operation() {
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
    let threads: Vec<_> = (0..8)
        .map(|_| {
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                super::system().expect("macOS supplies DNS-SD") as *const super::Api as usize
            })
        })
        .collect();
    let addresses: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
    assert!(addresses.windows(2).all(|pair| pair[0] == pair[1]));
}
