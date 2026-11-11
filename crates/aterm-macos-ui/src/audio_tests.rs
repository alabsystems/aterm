// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

#[test]
fn complete_table_is_shared_without_opening_an_audio_device() {
    std::thread::scope(|scope| {
        let workers: Vec<_> = (0..8).map(|_| scope.spawn(super::api)).collect();
        let tables = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect::<Vec<_>>();
        assert!(tables.iter().all(|table| std::ptr::eq(*table, tables[0])));
    });
}
