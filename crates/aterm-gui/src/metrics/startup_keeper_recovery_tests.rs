// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const fn spent(copies: u64, plants: u64, ms: u64) -> SeamSpent {
    SeamSpent {
        copies,
        plants,
        ns: ms * 1_000_000,
    }
}

/// The boot recovery's readings come from its stamps: `main` to the answered HELLO, and
/// from that HELLO to the admission's end, to every session placed, to their imports
/// settled, to the fence's last confirming SYNC written, to its lifting and to the first
/// control request past it, each less the seam's work (which all falls inside the
/// admission); the slowest one offer and the slowest one SYNC's answer as recorded.
/// Negative controls: no HELLO answered is no reading at all, whatever else was stamped; a
/// HELLO whose admission has not ended reads no admission, placement, import or fence
/// moment; no `main` stamp leaves only the HELLO's own reading out; no seam work leaves
/// every reading whole.
#[test]
fn the_keeper_recovery_reads_from_the_hello_less_the_seam_s_own_work() {
    let main = Instant::now();
    let hello = main + Duration::from_millis(7);
    let at = |ms| hello + Duration::from_millis(ms);
    let admitted = KeeperAdmitted {
        at: at(18),
        offers: 32,
        slowest_ns: 2_000_000,
        seam: spent(32, 32, 12),
    };
    let stamps = KeeperStamps {
        main: Some(main),
        hello: Some(hello),
        admitted: Some(admitted),
        placed: Some(at(20)),
        imported: Some(at(31)),
        fence: Some(keeper_fence([
            Some((at(19), at(23))),
            Some((at(21), at(22))),
        ])),
        served: Some(at(25)),
    };
    let k = keeper_recovery(stamps).expect("answered");
    assert_eq!(
        (k.offers, k.hello_ns, k.admit_ns, k.slowest_ns),
        (32, Some(7_000_000), Some(6_000_000), Some(2_000_000)),
        "the admission less the seam's 12 ms"
    );
    assert_eq!(
        (k.placed_ns, k.imported_ns),
        (Some(8_000_000), Some(19_000_000)),
        "placement and the imports less the seam's 12 ms"
    );
    assert_eq!(
        (
            k.fence_cohort,
            k.sync_sent_ns,
            k.sync_answer_max_ns,
            k.fence_ns,
            k.served_ns
        ),
        (
            Some(2),
            Some(9_000_000),
            Some(4_000_000),
            Some(11_000_000),
            Some(13_000_000)
        ),
        "the fence's moments less the seam's 12 ms, its slowest answer whole"
    );
    let unseamed = KeeperStamps {
        admitted: Some(KeeperAdmitted {
            seam: SeamSpent::default(),
            ..admitted
        }),
        ..stamps
    };
    let whole = keeper_recovery(unseamed).expect("answered");
    assert_eq!(
        (whole.admit_ns, whole.placed_ns, whole.imported_ns),
        (Some(18_000_000), Some(20_000_000), Some(31_000_000)),
        "no seam work, every reading whole"
    );
    assert_eq!(
        keeper_recovery(KeeperStamps {
            hello: None,
            ..stamps
        }),
        None,
        "no HELLO answered"
    );
    let pending = keeper_recovery(KeeperStamps {
        admitted: None,
        ..stamps
    })
    .expect("answered");
    assert_eq!(
        (
            pending.offers,
            pending.admit_ns,
            pending.slowest_ns,
            pending.placed_ns,
            pending.imported_ns
        ),
        (0, None, None, None, None),
        "an admission not ended reads nothing after the HELLO"
    );
    assert_eq!(
        (pending.sync_sent_ns, pending.fence_ns, pending.served_ns),
        (None, None, None),
        "nor any fence moment"
    );
    assert_eq!(
        keeper_recovery(KeeperStamps {
            main: None,
            ..stamps
        })
        .map(|k| (k.hello_ns, k.admit_ns)),
        Some((None, Some(6_000_000))),
        "no main stamp"
    );
}

/// The fence lifts at the last answer read once the keeper confirmed every authority's new
/// journal: the last confirming SYNC written and the slowest one answer come with it.
/// Negative controls: one authority unconfirmed keeps it shut whatever the others read, and
/// an empty cohort, whose fence never closed, has no lifting to read.
#[test]
fn the_fence_lifts_at_the_last_answer_once_every_authority_is_confirmed() {
    let t0 = Instant::now();
    let at = |ms| t0 + Duration::from_millis(ms);
    assert_eq!(
        keeper_fence([Some((at(1), at(5))), Some((at(3), at(4)))]),
        KeeperFence {
            cohort: 2,
            lifted: Some(FenceLift {
                last_sent: at(3),
                last_read: at(5),
                slowest_answer_ns: 4_000_000,
            }),
        }
    );
    assert_eq!(
        keeper_fence([Some((at(1), at(5))), None, Some((at(3), at(4)))]),
        KeeperFence {
            cohort: 3,
            lifted: None
        },
        "one authority unconfirmed"
    );
    assert_eq!(
        keeper_fence(std::iter::empty()),
        KeeperFence {
            cohort: 0,
            lifted: None
        },
        "no cohort"
    );
}

/// The ledger reads the fence off its cohort's own observers: shut while the keeper has
/// confirmed none, lifted at the confirmed read's answer once a receipt reporting to the
/// observer is confirmed.
#[cfg(unix)]
#[test]
fn the_ledger_reads_the_fence_off_its_cohort_s_observers() {
    use aterm_keeper::client::{CompanionReceipt, ReadObserver};
    use std::sync::Arc;
    let startup = Startup::new();
    assert_eq!(startup.keeper_fence(), None, "no cohort recorded");
    let observer = Arc::new(ReadObserver::default());
    let _ = startup.keeper_cohort.set(vec![Arc::clone(&observer)]);
    assert_eq!(
        startup.keeper_fence(),
        Some(KeeperFence {
            cohort: 1,
            lifted: None
        }),
        "unconfirmed"
    );
    CompanionReceipt::reporting_confirmation(Arc::clone(&observer)).confirm_captured();
    let read = observer.confirmation().expect("confirmed");
    assert_eq!(
        startup.keeper_fence().and_then(|f| f.lifted),
        Some(FenceLift {
            last_sent: read.sync_sent,
            last_read: read.answer_read,
            slowest_answer_ns: duration_ns(read.sync_sent, read.answer_read).expect("ordered"),
        })
    );
}

/// What the seam spent between two readings is their difference, never below zero.
#[test]
fn the_seam_s_work_since_a_reading_is_the_difference() {
    assert_eq!(spent(5, 3, 40).since(spent(2, 1, 15)), spent(3, 2, 25));
    assert_eq!(spent(1, 0, 1).since(spent(2, 1, 5)), SeamSpent::default());
    assert_eq!(spent(0, 0, 3).spent(), Duration::from_millis(3));
}

/// `metrics` spells the fields on every summary: `none` on a launch no keeper answered
/// (the negative control), the readings once the stamps are set (the fence's cohort on a
/// platform with a keeper, its SYNC and lift readings `none` while one authority is
/// unconfirmed), and in this build (which compiles the seams) what the seam did.
#[test]
fn metrics_spells_the_keeper_recovery_or_none() {
    let fields = |startup: &Startup| wire::render_text(|s| startup.emit_keeper_recovery(s));
    let unanswered = Startup::new();
    let _ = unanswered.rust_main.set(Instant::now());
    assert_eq!(
        fields(&unanswered),
        " startup_keeper_offers=none startup_keeper_hello_ms=none \
         startup_keeper_admit_ms=none startup_keeper_admit_max_ms=none \
         startup_keeper_placed_ms=none startup_keeper_imported_ms=none \
         startup_keeper_fence_cohort=none startup_keeper_sync_sent_ms=none \
         startup_keeper_sync_answer_max_ms=none startup_keeper_fence_ms=none \
         startup_keeper_served_ms=none startup_keeper_seam_copies=none \
         startup_keeper_seam_plants=none startup_keeper_seam_ms=none"
    );
    let answered = Startup::new();
    let main = Instant::now();
    let _ = answered.rust_main.set(main);
    let _ = answered.keeper_hello.set(main + Duration::from_millis(3));
    let _ = answered.keeper_admitted.set(KeeperAdmitted {
        at: main + Duration::from_millis(9),
        offers: 2,
        slowest_ns: 4_000_000,
        seam: spent(2, 1, 1),
    });
    let _ = answered.keeper_placed.set(main + Duration::from_millis(12));
    let _ = answered.keeper_served.set(main + Duration::from_millis(14));
    #[cfg(unix)]
    let _ = answered
        .keeper_cohort
        .set(vec![std::sync::Arc::default(), std::sync::Arc::default()]);
    let cohort = if cfg!(unix) { "2" } else { "none" };
    assert_eq!(
        fields(&answered),
        format!(
            " startup_keeper_offers=2 startup_keeper_hello_ms=3.00 \
             startup_keeper_admit_ms=5.00 startup_keeper_admit_max_ms=4.00 \
             startup_keeper_placed_ms=8.00 startup_keeper_imported_ms=none \
             startup_keeper_fence_cohort={cohort} startup_keeper_sync_sent_ms=none \
             startup_keeper_sync_answer_max_ms=none startup_keeper_fence_ms=none \
             startup_keeper_served_ms=10.00 startup_keeper_seam_copies=2 \
             startup_keeper_seam_plants=1 startup_keeper_seam_ms=1.00"
        )
    );
}
