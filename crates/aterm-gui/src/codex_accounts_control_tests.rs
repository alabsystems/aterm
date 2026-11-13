// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn schema_and_correlation_are_exact_and_do_not_admit_extra_authority() {
    assert!(request("snapshot schema=1 request=0123456789abcdef0123456789abcdef").is_some());
    for bad in [
        "snapshot schema=2 request=0123456789abcdef0123456789abcdef",
        "snapshot schema=1 request=0123456789ABCDEF0123456789abcdef",
        "snapshot schema=1 request=short",
        "snapshot schema=1 request=0123456789abcdef0123456789abcdef target=other",
    ] {
        assert!(request(bad).is_none());
    }
}

#[test]
fn display_metadata_does_not_publish_addresses_or_credential_routes() {
    assert_eq!(
        masked_email(Some("person@example.com")).as_deref(),
        Some("p***@example.com")
    );
    assert_eq!(masked_email(Some("person\n@example.com")), None);
    assert_eq!(account_id("work"), account_id("work"));
    assert_ne!(account_id("work"), account_id("home"));
}

#[test]
fn observation_needs_no_resume_record_and_default_route_proves_no_assignment() {
    let root = aterm_tempfile::tempdir().unwrap();
    let mut env = Env::from_process().unwrap();
    env.home = Some(root.path().join("home"));
    env.state = root.path().join("state");
    env.config = Some(root.path().join("config/aterm.toml"));
    std::fs::create_dir_all(root.path().join("config")).unwrap();
    std::fs::write(
        root.path().join("config/accounts-codex.toml"),
        "[[account]]\nlabel='main'\nclass='dedicated'\n",
    )
    .unwrap();
    let proof = crate::self_claim::Proven {
        pid: 42,
        start: "captured-kernel-birth".into(),
    };
    let value = snapshot_in("bound-session", &proof, &env).unwrap();
    assert_eq!(value["assignment"]["account_id"], Value::Null);
    assert_eq!(value["assignment"]["mode"].as_str(), Some("unknown"));
    assert_eq!(value["accounts"][0]["eligible"].as_bool(), Some(false));
    assert_eq!(
        value["session"]["process_birth"].as_str(),
        Some("captured-kernel-birth")
    );
    assert!(
        !root.path().join("home").exists(),
        "no resume or credential home was read or created"
    );
    assert!(!env.state.exists(), "observation creates no account state");
    let mut later = env.clone();
    later.now += 10;
    let again = snapshot_in("bound-session", &proof, &later).unwrap();
    assert_eq!(
        value["revision"], again["revision"],
        "clock ticks do not stale authority metadata"
    );
    assert_ne!(value["observed_at"], again["observed_at"]);
}

#[test]
fn missing_or_replaced_caller_never_receives_a_snapshot() {
    let proof = crate::self_claim::Proven {
        pid: 42,
        start: "birth-a".into(),
    };
    let replaced = crate::self_claim::Proven {
        pid: 42,
        start: "birth-b".into(),
    };
    assert!(same_binding(&proof, Some(&proof), true));
    assert!(!same_binding(&proof, None, true));
    assert!(!same_binding(&proof, Some(&replaced), true));
    assert!(!same_binding(&proof, Some(&proof), false));
}

#[test]
fn live_workers_keep_their_bounded_slot_after_request_expiry() {
    let mut table = Jobs::new();
    for i in 0..32 {
        table.insert(
            ("sid".into(), format!("{i:032x}")),
            Arc::new(Job {
                proof: crate::self_claim::Proven {
                    pid: 42,
                    start: "birth".into(),
                },
                started: Instant::now() - Duration::from_secs(121),
                result: Mutex::new(None),
            }),
        );
    }
    prune(&mut table);
    assert_eq!(table.len(), 32);
    let key = ("sid".into(), format!("{:032x}", 0));
    let first = Arc::clone(table.get(&key).unwrap());
    assert!(
        Arc::ptr_eq(&first, table.get(&key).unwrap()),
        "same request polls one worker"
    );
    *first.result.lock().unwrap() = Some(None);
    prune(&mut table);
    assert_eq!(
        table.len(),
        31,
        "only a terminal expired worker releases its slot"
    );
}
