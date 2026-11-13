// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_capacity::host_protocol::{Generation, Job, Phase};

#[test]
fn epoch_witness_requires_exact_live_and_settled_backing_beside_the_same_peer() {
    let mut snapshot = Snapshot {
        generation: Generation([7; 16]),
        revision: 8,
        closed: false,
        jobs: vec![Job {
            attempt: 9,
            phase: Phase::Waiting,
            reserved_bytes: 123456,
            failure: None,
        }],
        epochs: vec![Epoch {
            sequence: 1,
            backing: 2,
            roots: 1,
            reserved_bytes: 234567,
            opened: true,
            retired: false,
            helper_exited: false,
            control_eof: false,
            endpoint_cleaned: false,
            settled_units: 0,
            uncertain_units: 0,
            remaining_units: 2,
            completed_roots: 0,
            failed_roots: 0,
            faulted: false,
            unbound: false,
            not_spawned: false,
            cancellation_started: false,
            failure: None,
            cache_work: Vec::new(),
        }],
    };
    let base = concat!(
        "text key=\"allocation/monitor/scope\" label=\"Delegated native session · read-only observation\"\n",
        "text key=\"allocation/monitor/attachment\" label=\"Monitor attachment 4: Observed\"\n",
        "text key=\"allocation/monitor/source\" label=\"Source generation: 07070707070707070707070707070707 · snapshot revision 8 · attachment 4\"\n",
        "text key=\"allocation/monitor/attempt/9\" label=\"Session attempt 9 · Waiting for next child observation · admitted peak 123456 bytes (0.0 GiB)\"\n",
    );
    let live = concat!(
        "text key=\"allocation/monitor/epoch/1/budget\" label=\"Epoch 1 · CPU backing N=2 · root slots R=1 · original admitted memory peak 234567 bytes (0.0 GiB)\"\n",
        "text key=\"allocation/monitor/epoch/1/roots\" label=\"Roots: 0 completed of 1 slots · 0 failed\"\n",
        "text key=\"allocation/monitor/epoch/1/transport\" label=\"Transport: opened yes · Retired no · helper exited no · output ended no\"\n",
        "text key=\"allocation/monitor/epoch/1/backing\" label=\"CPU backing: 0 returned · 2 remaining · 0 uncertain · group drain + endpoint cleanup pending\"\n",
        "text key=\"allocation/monitor/epoch/1/cache/not-recorded\" label=\"Cache validation: no completed-root report recorded\"\n",
    );
    let text = format!("{base}{live}");
    assert!(matches_snapshot(&text, &snapshot, 4));
    assert!(!matches_snapshot(base, &snapshot, 4), "missing epoch rows");
    for (before, after) in [
        ("epoch/1/", "epoch/2/"),
        ("Epoch 1 ·", "Epoch 2 ·"),
        ("N=2", "N=3"),
        ("R=1", "R=2"),
        ("234567", "234568"),
        ("0 completed", "1 completed"),
        ("of 1 slots", "of 2 slots"),
        ("0 failed", "1 failed"),
        ("opened yes", "opened no"),
        ("Retired no", "Retired yes"),
        ("helper exited no", "helper exited yes"),
        ("output ended no", "output ended yes"),
        ("0 returned", "2 returned"),
        ("2 remaining", "0 remaining"),
        ("0 uncertain", "1 uncertain"),
        ("cleanup pending", "cleanup confirmed"),
        ("Session attempt 9", "Session attempt 10"),
        ("snapshot revision 8", "snapshot revision 9"),
        ("no completed-root report recorded", "0 bytes hashed"),
    ] {
        assert!(
            !matches_snapshot(&text.replace(before, after), &snapshot, 4),
            "reject {after}"
        );
    }
    assert!(
        !matches_snapshot(&format!("{text}{live}"), &snapshot, 4),
        "duplicate epoch rows"
    );
    assert!(
        !matches_snapshot(
            &format!("{text}{}", live.replace("epoch/1/", "epoch/2/")),
            &snapshot,
            4
        ),
        "extra epoch rows"
    );
    let audit = format!(
        "{}{}",
        text.replace("text key=", "paint-node visible=true clipped=false key="),
        text.replace(
            "text key=",
            "paint-text overflow=false clip-truncated=false key="
        )
        .replace("label=", "painted=")
    );
    assert!(matches_snapshot(&audit, &snapshot, 4));
    assert!(
        !matches_snapshot(
            &audit.replace(
                "painted=\"CPU backing: 0 returned",
                "painted=\"CPU backing: 2 returned"
            ),
            &snapshot,
            4
        ),
        "painted custody must match semantic custody"
    );
    assert!(
        !matches_snapshot(
            &audit.replace("clipped=false", "clipped=true"),
            &snapshot,
            4
        ),
        "clipped epochs never qualify"
    );
    let epoch = &mut snapshot.epochs[0];
    epoch.retired = true;
    epoch.helper_exited = true;
    epoch.control_eof = true;
    epoch.endpoint_cleaned = true;
    epoch.completed_roots = 1;
    epoch.settled_units = 2;
    epoch.remaining_units = 0;
    let settled = text
        .replace("Roots: 0 completed", "Roots: 1 completed")
        .replace("Retired no", "Retired yes")
        .replace("helper exited no", "helper exited yes")
        .replace("output ended no", "output ended yes")
        .replace("0 returned", "2 returned")
        .replace("2 remaining", "0 remaining")
        .replace("cleanup pending", "cleanup confirmed")
        .replace(
            "no completed-root report recorded",
            "not recorded for 1 completed roots",
        );
    assert!(matches_snapshot(&settled, &snapshot, 4));
    assert!(
        !matches_snapshot(&text, &snapshot, 4),
        "live work is not settled history"
    );
    snapshot.epochs[0].settled_units = 1;
    snapshot.epochs[0].uncertain_units = 1;
    assert!(
        !matches_snapshot(&settled, &snapshot, 4),
        "uncertain repayment is never all-N success"
    );
    assert!(matches_snapshot(
        &settled
            .replace("2 returned", "1 returned")
            .replace("0 uncertain", "1 uncertain"),
        &snapshot,
        4
    ));
}

#[test]
fn synthetic_cache_witness_requires_exact_run_costs_and_all_availability_rows() {
    use aterm_capacity::{
        cache_work::{Availability, Denominators, Summary, UnavailableReason, Work},
        host_protocol::RootCacheWork,
    };
    let complete = Summary {
        run_id: "20261105T103732783Z-1ab3ba0487a929a0".into(),
        availability: Availability::Complete,
        denominators: Denominators {
            registered_units: 7,
            run_script_units: 2,
            observed_units: 1,
            units_without_attempts: 6,
            attempts_started: 2,
            attempts_finished: 2,
            captured_attempts: 1,
            uncacheable_attempts: 1,
        },
        totals: Some(Work {
            entries: 3,
            input_bytes: 41,
            bytes_hashed: 17,
            metadata_reads: 12,
            resolution_steps: 11,
            link_reads: 0,
            observations: 15,
            revalidations: 7,
        }),
    };
    let legacy = Summary {
        run_id: "20261105T103732784Z-1ab3ba0487a929a0".into(),
        availability: Availability::UnavailableLegacyLog,
        denominators: Denominators {
            registered_units: 7,
            run_script_units: 2,
            units_without_attempts: 7,
            ..Denominators::default()
        },
        totals: None,
    };
    // This is an independent semantic law fixture. The real-window scene
    // currently has no analysis report and must show honest NotRecorded.
    let snapshot = Snapshot {
        generation: Generation([7; 16]),
        revision: 8,
        closed: false,
        jobs: vec![],
        epochs: vec![Epoch {
            sequence: 1,
            backing: 4,
            roots: 4,
            reserved_bytes: 42,
            opened: true,
            retired: true,
            helper_exited: true,
            control_eof: true,
            endpoint_cleaned: true,
            settled_units: 4,
            uncertain_units: 0,
            remaining_units: 0,
            completed_roots: 4,
            failed_roots: 0,
            faulted: false,
            unbound: false,
            not_spawned: false,
            cancellation_started: false,
            failure: None,
            cache_work: vec![
                RootCacheWork {
                    root: 0,
                    summary: Some(complete),
                    unavailable: None,
                },
                RootCacheWork {
                    root: 1,
                    summary: Some(legacy),
                    unavailable: None,
                },
                RootCacheWork {
                    root: 2,
                    summary: None,
                    unavailable: Some(UnavailableReason::QueryFailed),
                },
            ],
        }],
    };
    snapshot.encode().unwrap();
    let text = concat!(
        "text key=\"allocation/monitor/scope\" label=\"Delegated native session · read-only observation\"\n",
        "text key=\"allocation/monitor/attachment\" label=\"Monitor attachment 4: Observed\"\n",
        "text key=\"allocation/monitor/source\" label=\"Source generation: 07070707070707070707070707070707 · snapshot revision 8 · attachment 4\"\n",
        "text key=\"allocation/monitor/epoch/1/budget\" label=\"Epoch 1 · CPU backing N=4 · root slots R=4 · original admitted memory peak 42 bytes (0.0 GiB)\"\n",
        "text key=\"allocation/monitor/epoch/1/roots\" label=\"Roots: 4 completed of 4 slots · 0 failed\"\n",
        "text key=\"allocation/monitor/epoch/1/transport\" label=\"Transport: opened yes · Retired yes · helper exited yes · output ended yes\"\n",
        "text key=\"allocation/monitor/epoch/1/backing\" label=\"CPU backing: 4 returned · 0 remaining · 0 uncertain · group drain + endpoint cleanup confirmed\"\n",
        "text key=\"allocation/monitor/epoch/1/root/0/cache/availability\" label=\"Root 0 · Historical cache validation · run 20261105T103732783Z-1ab3ba0487a929a0 · scope build-script-inputs-v1 · complete\"\n",
        "text key=\"allocation/monitor/epoch/1/root/0/cache/coverage\" label=\"Build-script input work: 1 observed of 2 script units · 7 registered units · 2 capture attempts · 1 uncacheable\"\n",
        "text key=\"allocation/monitor/epoch/1/root/0/cache/work\" label=\"Inputs: 17 bytes hashed · 12 metadata reads · 7 revalidations\"\n",
        "text key=\"allocation/monitor/epoch/1/root/1/cache/availability\" label=\"Root 1 · Historical cache validation · run 20261105T103732784Z-1ab3ba0487a929a0 · scope build-script-inputs-v1 · legacy log unavailable\"\n",
        "text key=\"allocation/monitor/epoch/1/root/2/cache/availability\" label=\"Root 2 · Historical cache validation: unavailable · report query failed\"\n",
        "text key=\"allocation/monitor/epoch/1/cache/not-recorded\" label=\"Cache validation: not recorded for 1 completed roots\"\n",
    );
    assert!(matches_snapshot(text, &snapshot, 4));
    for (before, after) in [
        ("20261105T103732783Z", "20261105T103732782Z"),
        ("1ab3ba0487a929a0", "2ab3ba0487a929a0"),
        ("root/0/cache/", "root/9/cache/"),
        ("17 bytes hashed", "18 bytes hashed"),
        ("1 observed of 2", "2 observed of 2"),
        ("12 metadata reads", "13 metadata reads"),
        ("legacy log unavailable", "complete"),
        ("report query failed", "not recorded"),
        ("not recorded for 1", "not recorded for 0"),
    ] {
        assert!(
            !matches_snapshot(&text.replace(before, after), &snapshot, 4),
            "reject {after}"
        );
    }
    let missing = text
        .lines()
        .filter(|line| !line.contains("root/0/cache/work"))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!matches_snapshot(&missing, &snapshot, 4));
    let extra = format!(
        "{text}text key=\"allocation/monitor/epoch/1/root/1/cache/work\" label=\"Inputs: 0 bytes hashed · 0 metadata reads · 0 revalidations\"\n"
    );
    assert!(!matches_snapshot(&extra, &snapshot, 4));
    let audit = format!(
        "{}{}",
        text.replace("text key=", "paint-node visible=true clipped=false key="),
        text.replace(
            "text key=",
            "paint-text overflow=false clip-truncated=false key="
        )
        .replace("label=", "painted=")
    );
    assert!(matches_snapshot(&audit, &snapshot, 4));
    assert!(!matches_snapshot(
        &audit.replace("painted=\"Inputs: 17", "painted=\"Inputs: 18"),
        &snapshot,
        4
    ));
}

#[test]
fn scene_requires_exact_owner_completion_beside_current_snapshot() {
    let snapshot = Snapshot {
        generation: Generation([7; 16]),
        revision: 7,
        closed: false,
        jobs: vec![],
        epochs: vec![],
    };
    let recent = [RecordSummary {
        revision: 6,
        attempt: Some(8),
        kind: EventKind::Finished {
            succeeded: true,
            cancelled: false,
        },
    }];
    let scene = Scene {
        snapshot: &snapshot,
        recent: &recent,
    };
    let base = concat!(
        "text key=\"allocation/monitor/scope\" label=\"Delegated native session · read-only observation\"\n",
        "text key=\"allocation/monitor/attachment\" label=\"Monitor attachment 4: Observed\"\n",
        "text key=\"allocation/monitor/source\" label=\"Source generation: 07070707070707070707070707070707 · snapshot revision 7 · attachment 4\"\n",
    );
    let completion = "text key=\"allocation/monitor/completion/6\" label=\"Session attempt 8 · Finished · success established · completion revision 6\"\n";
    let text = format!("{base}{completion}");
    assert!(matches_scene(&text, &scene, 4));
    assert!(!matches_scene(base, &scene, 4), "missing actual outcome");
    assert!(!matches_scene(&format!("{text}{completion}"), &scene, 4));
    for (from, to) in [
        ("Session attempt 8", "Session attempt 80"),
        ("success established", "success not established"),
        (
            " · completion revision",
            " · cancellation requested · completion revision",
        ),
        ("completion revision 6", "completion revision 5"),
        ("completion/6", "completion/5"),
    ] {
        assert!(
            !matches_scene(&text.replace(from, to), &scene, 4),
            "reject {to}"
        );
    }
    assert!(
        !matches_scene(
            &text,
            &Scene {
                snapshot: &snapshot,
                recent: &[]
            },
            4
        ),
        "extra outcome"
    );
    let audit = format!(
        "{}{}",
        text.replace("text key=", "paint-node visible=true clipped=false key="),
        text.replace(
            "text key=",
            "paint-text overflow=false clip-truncated=false key="
        )
        .replace("label=", "painted=")
    );
    assert!(matches_scene(&audit, &scene, 4));
    assert!(
        !matches_scene(
            &audit.replace(
                "painted=\"Session attempt 8",
                "painted=\"Session attempt 80"
            ),
            &scene,
            4
        ),
        "paint differs from semantic outcome"
    );
}

#[test]
fn view_parser_uses_authoritative_tab_index_and_allocation_leaf() {
    assert_eq!(
        allocation_view("tab id=99 index=2\n leaf view=12 kind=app app=allocation\n"),
        Some((12, 2))
    );
    assert_eq!(
        allocation_view("tab id=99 index=2\n leaf view=12 kind=app app=editor\n"),
        None
    );
}
#[test]
fn rendered_witness_requires_actual_generation_attempts_and_admitted_bytes() {
    let expected = Snapshot {
        generation: Generation([7; 16]),
        revision: 3,
        closed: false,
        jobs: vec![Job {
            attempt: 9,
            phase: Phase::Waiting,
            reserved_bytes: 123456,
            failure: None,
        }],
        epochs: vec![],
    };
    let text = concat!(
        "text key=\"allocation/monitor/scope\" label=\"Delegated native session · read-only observation\"\n",
        "text key=\"allocation/monitor/attachment\" label=\"Monitor attachment 4: Observed\"\n",
        "text key=\"allocation/monitor/source\" label=\"Source generation: 07070707070707070707070707070707 · snapshot revision 3 · attachment 4\"\n",
        "text key=\"allocation/monitor/attempt/9\" label=\"Session attempt 9 · Waiting for next child observation · admitted peak 123456 bytes (0.0 GiB)\"\n",
    );
    assert!(matches_snapshot(text, &expected, 4));
    let audit = format!(
        "{}{}",
        text.replace("text key=", "paint-node visible=true clipped=false key="),
        text.replace(
            "text key=",
            "paint-text overflow=false clip-truncated=false key="
        )
        .replace("label=", "painted=")
    );
    assert!(
        matches_snapshot(&audit, &expected, 4),
        "semantic and painted rows are separate matching witnesses"
    );
    for (valid, invalid) in [
        ("visible=true", "visible=false"),
        ("clipped=false", "clipped=true"),
        ("overflow=false", "overflow=true"),
        ("clip-truncated=false", "clip-truncated=true"),
    ] {
        assert!(
            !matches_snapshot(&audit.replace(valid, invalid), &expected, 4),
            "reject {invalid}"
        );
    }
    assert!(!matches_snapshot(&audit.replace("painted=\"Session attempt 9 · Waiting for next child observation · admitted peak 123456", "painted=\"Session attempt 9 · Waiting for next child observation · admitted peak 654321"), &expected, 4));
    assert!(!matches_snapshot(
        &text.replace("snapshot revision 3", "snapshot revision 2"),
        &expected,
        4
    ));
    assert!(!matches_snapshot(
        &text.replace("123456", "123455"),
        &expected,
        4
    ));
    assert!(!matches_snapshot(
        &text.replace("attempt 9", "attempt 8"),
        &expected,
        4
    ));
    assert!(!matches_snapshot(
        &text.replace(
            "07070707070707070707070707070707",
            "00000000000000000000000000000000"
        ),
        &expected,
        4
    ));
    assert!(!matches_snapshot(text, &expected, 5));
    assert!(!matches_snapshot(
        &text.replace("· attachment 4\"", "· attachment 40\""),
        &expected,
        4
    ));
    assert!(!matches_snapshot(
        &text.replace("Waiting for next child observation", "Faulted"),
        &expected,
        4
    ));
    assert!(!matches_snapshot(
        &format!(
            "{text}\ntext key=\"allocation/monitor/attempt/10\" label=\"Session attempt 10 · Waiting for next child observation · admitted peak 999 bytes (0.0 GiB)\""
        ),
        &expected,
        4
    ));
    assert!(!matches_snapshot(
        &text.replace("attempt 9 ·", "attempt 90 ·"),
        &expected,
        4
    ));
    assert!(!matches_snapshot(
        &text.replace("· admitted peak", "\n· admitted peak"),
        &expected,
        4
    ));
    assert!(
        !matches_snapshot(&format!("{text}{text}"), &expected, 4),
        "duplicate semantic keys refuse"
    );
}
