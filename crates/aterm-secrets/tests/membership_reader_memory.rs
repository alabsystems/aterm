// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A warm membership load reads current bytes without allocating the whole stream again.

#![cfg(unix)]

use aterm_secrets::{
    SecretBytes,
    age::Identity,
    client::{Paths, load_records},
    commons::binding::Request,
    fsx,
    membership_log::{Entrant, EpochAudience, Inserted, Log, Record, read_records},
    text::{Audience, Entry},
};
use std::cell::Cell;

#[path = "memory_alloc/net_peak.rs"]
mod net_peak;

use net_peak::{ACTIVE, LIVE, PEAK};

struct Measuring;

impl Drop for Measuring {
    fn drop(&mut self) {
        ACTIVE.with(|active| active.set(false));
    }
}

fn large_history() -> Log {
    let seed = [17; 32];
    let incarnation = [18; 16];
    let identity = Identity::from_secret_bytes([19; 32]);
    let request =
        Request::create([20; 16], [21; 32], [22; 16], incarnation, &identity, &seed).unwrap();
    let actor = request.binding().actor();
    let genesis = Record::found(
        &[23; 32],
        vec![Entrant {
            request,
            node_key: [24; 32],
            name: "reader".into(),
            label: "m1".into(),
        }],
        1,
    )
    .unwrap();
    let mut log = Log::new(genesis).unwrap();
    let (draft, key) = log
        .draft_epoch(&actor, 2, EpochAudience::Everyone, 1)
        .unwrap();
    let epoch = draft.sign(&seed, incarnation).unwrap();
    let epoch_id = *epoch.id();
    assert_eq!(log.insert(epoch).unwrap(), Inserted::Held);
    for i in 0..6 {
        let entry = Entry {
            name: format!("value-{i}"),
            value: SecretBytes::new(vec![i as u8; 512 * 1024]),
            audience: Audience::Everyone,
            if_absent: false,
            anchor: None,
        };
        let record = log
            .draft_put(&actor, 3 + i, epoch_id, &key, &entry)
            .unwrap()
            .sign(&seed, incarnation)
            .unwrap();
        assert_eq!(log.insert(record).unwrap(), Inserted::Held);
    }
    log
}

#[test]
fn a_warm_membership_load_uses_bounded_stream_scratch() {
    let home = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(home.path());
    fsx::ensure_private_dir(&paths.dir).unwrap();
    let log = large_history();
    let wire = read_records(log.records()).unwrap();
    assert!(wire.len() > 3 * 1024 * 1024);
    fsx::write_atomic(&paths.records_file(), &wire, 0o600).unwrap();
    let warm = load_records(&paths).unwrap();
    assert_eq!(warm.complete_inventory().len(), log.held_count());
    assert_eq!(warm.read_encoded().unwrap(), wire);
    drop(warm);

    LIVE.with(|live| live.set(0));
    PEAK.with(|peak| peak.set(0));
    ACTIVE.with(|active| assert!(!active.replace(true)));
    let measuring = Measuring;
    let observed = load_records(&paths);
    let count = observed.as_ref().unwrap().len();
    drop(observed);
    drop(measuring);
    assert_eq!(LIVE.with(Cell::get), 0, "all measured owners released");
    assert_eq!(count, log.held_count());
    let peak = PEAK.with(Cell::get);
    let bound = 256 * 1024;
    assert!(
        peak <= bound,
        "warm membership read peak {peak} exceeds {bound}"
    );
    println!(
        "warm membership bytes={} records={count} peak={peak} bound={bound}",
        wire.len()
    );
}
