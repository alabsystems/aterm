// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn receipt(route: Route) -> Receipt {
    let world = PathBuf::from("/private/tmp/.ubd/custody-test.noindex");
    Receipt {
        socket: world.join("run/a.sock"),
        owner: Identity::parse("1001@100:1").unwrap(),
        keeper: Identity::parse("1002@200:2").unwrap(),
        gui: Identity::parse("1003@300:3").unwrap(),
        lane: (1, 2),
        memory: crate::window_guards::MemoryObservation {
            claim: "400-1001-5.claim".into(),
            device: 1,
            inode: 3,
            bytes: MEMORY,
            pid: 1001,
            start_ms: 400,
            sequence: 5,
        },
        route,
        job: (route == Route::AppBand).then(|| crate::band::label_for(&world, 1001)),
        world,
    }
}

#[test]
fn custody_receipt_binds_route_job_process_roles_and_original_memory() {
    for route in [Route::Child, Route::AppBand] {
        let original = receipt(route).text();
        assert_eq!(
            Receipt::parse(original.as_bytes()).unwrap().text(),
            original
        );
        let bad_route = if route == Route::Child {
            "app"
        } else {
            "agent"
        };
        let bad_job = if route == Route::Child {
            "aterm-lab.instance.foreign.1001"
        } else {
            "none"
        };
        for changed in [
            original.replace(
                &format!("route={}\n", route.name()),
                &format!("route={bad_route}\n"),
            ),
            original.replace(
                &format!("job={}\n", receipt(route).job.as_deref().unwrap_or("none")),
                &format!("job={bad_job}\n"),
            ),
            original.replace("keeper=1002@200:2", "keeper=1001@100:1"),
            original.replace("gui=1003@300:3", "gui=1002@200:2"),
            original.replace("memory-pid=1001", "memory-pid=2001"),
            original.replace(
                "memory-claim=400-1001-5.claim",
                "memory-claim=400-1001-6.claim",
            ),
            original.replace(
                "socket=/private/tmp/.ubd/custody-test.noindex/run/a.sock",
                "socket=/private/tmp/foreign.sock",
            ),
        ] {
            assert!(Receipt::parse(changed.as_bytes()).is_err(), "{changed}");
        }
    }
}

#[test]
fn launchd_absence_ack_never_becomes_a_child_wait_receipt() {
    let child = receipt(Route::Child);
    let job = receipt(Route::AppBand);
    let child_ack = child.keeper_ack();
    let job_ack = job.keeper_ack();
    assert_ne!(child_ack, job_ack);
    assert!(child_ack.contains("evidence=owned-child-wait-sigkill\n"));
    assert!(job_ack.contains("evidence=sigkill-delivered-native-absent\n"));
    let mut substituted = receipt(Route::AppBand);
    substituted.keeper = Identity::parse("1004@400:4").unwrap();
    assert_ne!(job_ack, substituted.keeper_ack());
    substituted = receipt(Route::AppBand);
    substituted.job = Some("aterm-lab.instance.foreign.1001".into());
    assert!(substituted.validate().is_err());
    assert_ne!(job_ack, substituted.keeper_ack());
}
