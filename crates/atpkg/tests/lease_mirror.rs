// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! THE LEASE MIRROR'S PIN (2026-09-26).
//!
//! The merge gate holds a lease on the toolchain it resolved for the whole run, and it
//! takes it with std alone: `aterm_verify::lease` mirrors `atpkg::lease`, because
//! aterm-verify has no dependencies by charter. A mirror that drifts from the protocol is
//! a lease nobody reads — the run is unprotected, and nothing says so. So this drives the
//! REAL mirror against the readers that matter: atpkg's `holders` (what gc and the flip
//! gate ask), its `reclaim` (what gc does before a delete), both subject kinds, and the
//! gate in the other direction — a reclaim in progress makes the mirror wait and say so.

use std::path::PathBuf;
use std::time::Duration;

use aterm_verify::lease::{Taken, WAIT, take};
use atpkg::lease::{Holders, Reclaim, Subject};

fn prefix(label: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("atpkg-lease-mirror-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    p
}

#[test]
fn the_merge_gates_lease_is_one_atpkg_reads_and_keeps() {
    let p = prefix("gate");
    for (dir, subject) in [
        (
            p.join("store/trust/9192/bin"),
            Subject::build("trust", 9192).unwrap(),
        ),
        (p.join("rustup/trust/bin"), Subject::view("trust").unwrap()),
    ] {
        std::fs::create_dir_all(&dir).unwrap();
        let who = "aterm-verify (pid 7) \u{2014} the merge contract in /w";
        let Taken::Held(lease) = take(&p, &dir, who, WAIT) else {
            panic!("the mirror leases {}", subject.describe());
        };
        assert_eq!(
            lease.what,
            subject.describe(),
            "the two sides name it alike"
        );
        assert_eq!(
            atpkg::lease::holders(&p, &subject),
            Holders::Held(vec![who.to_string()]),
            "atpkg reads the mirror's lease, holder and all"
        );
        assert!(
            matches!(atpkg::lease::reclaim(&p, &subject), Reclaim::Keep(_)),
            "gc keeps what the gate holds"
        );
        let live = atpkg::lease::live(&p).unwrap();
        assert_eq!(live.len(), 1, "{live:?}");
        drop(lease);
        assert_eq!(atpkg::lease::holders(&p, &subject), Holders::Free);
    }
    let _ = std::fs::remove_dir_all(&p);
}

#[test]
fn a_reclaim_in_progress_holds_the_mirror_at_the_gate() {
    let p = prefix("reclaim");
    let bin = p.join("store/trust/9192/bin");
    std::fs::create_dir_all(&bin).unwrap();
    let subject = Subject::build("trust", 9192).unwrap();
    let Reclaim::Clear(guard) = atpkg::lease::reclaim(&p, &subject) else {
        panic!("nothing holds it yet");
    };
    let taken = take(&p, &bin, "late", Duration::from_millis(250));
    assert!(
        matches!(taken, Taken::Failed(ref why) if why.contains("reclaiming")),
        "{taken:?}"
    );
    drop(guard);
    assert!(matches!(take(&p, &bin, "after", WAIT), Taken::Held(_)));
    let _ = std::fs::remove_dir_all(&p);
}
