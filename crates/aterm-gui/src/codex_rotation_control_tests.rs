// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_current_terminal_refusal_ends_rotation_but_stale_and_transient_results_retry() {
    let now = Instant::now();
    for why in [
        "source-unbound",
        "source-missing",
        "source-unmanaged",
        "not-walled",
    ] {
        assert!(
            matches!(
                progress_after_observation(Err(why), true, now),
                Progress::NotNeeded
            ),
            "{why}"
        );
        assert!(
            matches!(progress_after_observation(Err(why), false, now), Progress::Retry(at) if at > now),
            "stale {why}"
        );
    }
    for result in [Err("roster-busy"), Err("probe-panicked"), Ok(None)] {
        assert!(
            matches!(progress_after_observation(result, true, now), Progress::Retry(at) if at > now)
        );
    }
}

fn job(proof: &crate::self_claim::Proven) -> Preparation {
    Preparation {
        sid: "rotation-test".into(),
        intent: Intent {
            purpose: renewal::Purpose::WeeklyQuota,
            thread: "00000000-0000-0000-0000-000000000001".into(),
            wall: "00000000-0000-0000-0000-000000000002".into(),
            identity: renewal::Identity::from_native("source", "user"),
        },
        process: Process {
            pid: proof.pid,
            birth: 10,
            parent: 42,
            group: proof.pid,
            session: 42,
            image: Image {
                path: b"/codex".to_vec(),
                device: 1,
                inode: 2,
            },
        },
        start: proof.start.clone(),
        human_seq: 3,
        starts: 1,
        progress: Mutex::new(Progress::Retry(Instant::now())),
    }
}

#[test]
fn codex_rotation_probe_cannot_follow_a_replacement_process_wall_or_person_turn() {
    let proof = crate::session_timeline::scoped_input_tests::scoped(&[Scope::Relaunch]).proven;
    let job = job(&proof);
    assert!(job.matches(&job.intent, &job.process, &proof, 3, 1));
    let mut intent = job.intent.clone();
    intent.wall = "00000000-0000-0000-0000-000000000003".into();
    assert!(!job.matches(&intent, &job.process, &proof, 3, 1));
    intent = job.intent.clone();
    intent.identity = renewal::Identity::from_native("replacement", "user");
    assert!(!job.matches(&intent, &job.process, &proof, 3, 1));
    let mut process = job.process.clone();
    process.birth += 1;
    assert!(!job.matches(&job.intent, &process, &proof, 3, 1));
    process = job.process.clone();
    process.image.inode += 1;
    assert!(!job.matches(&job.intent, &process, &proof, 3, 1));
    assert!(!job.matches(&job.intent, &job.process, &proof, 4, 1));
    assert!(!job.matches(&job.intent, &job.process, &proof, 3, 2));
}

#[test]
fn codex_rotation_index_retains_source_intent_across_candidate_changes() {
    let proof = crate::session_timeline::scoped_input_tests::scoped(&[Scope::Relaunch]).proven;
    let mut job = job(&proof);
    let original = index(&job).unwrap();
    // A destination is deliberately not in the key: a lost Ready response
    // must find its original receipt before another candidate can be chosen.
    *job.progress.lock().unwrap() = Progress::NotNeeded;
    assert_eq!(index(&job).unwrap(), original);
    job.intent.wall = "00000000-0000-0000-0000-000000000003".into();
    assert_ne!(index(&job).unwrap(), original);
    job.intent.wall = "00000000-0000-0000-0000-000000000002".into();
    job.process.birth += 1;
    assert_ne!(index(&job).unwrap(), original);
}
