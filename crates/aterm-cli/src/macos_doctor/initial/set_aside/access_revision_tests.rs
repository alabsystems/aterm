// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_types::macos_setup::{Access, AccessOutcome, access_record};

fn permissions(on: bool) -> Plan {
    let mut selection = Selection::none();
    selection.set(Setting::Permissions, on);
    Plan::from(selection)
}

fn choose(root: &Path, plan: &Plan) -> u64 {
    observe_mutation_at(root, None, Err("busy".into()), |root| {
        InitialSetupRestart::chosen().persist_with(
            root,
            plan,
            false,
            || Ok(None),
            save_progress,
            |root| observation_at(root).map_err(|error| error.to_string()),
        )
    })
    .result
    .unwrap()
    .expect("the group choice is admitted")
}

fn permission_rows(state: &Path, revision: u64) -> Vec<Row> {
    let records = access_record::read(state, revision);
    let mut grants = access::Grants {
        fda: access::Fda::NotConfirmed,
        stable: true,
        devtools: access::DevTools::On,
        debugger: access::Debugger::On,
        lan: access::Lan::On,
        accessibility: None,
        opted_out: [None; 5],
    };
    grants.opted_out = access::opted_out(
        [None; 5],
        &records,
        Access::ALL.map(|access| grants.reads_off(access)),
    );
    access::stage_rows(&grants, &records)
}

fn check_reapproval(damaged: bool) {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let state = scratch.path();
    let root = state.join("doctor");
    let on = permissions(true);
    let off = permissions(false);
    choose(&root, &on);
    choose(&root, &off);
    choose(&root, &on);
    let old_approval = access_revision_at(&root).unwrap();
    access_record::append(
        state,
        old_approval,
        Access::FullDiskAccess,
        AccessOutcome::Declined,
    )
    .unwrap();
    assert!(
        permission_rows(state, old_approval)
            .iter()
            .all(|row| row.status == "ok"),
        "Not now completes its own approval"
    );

    if damaged {
        std::fs::write(root.join(PROGRESS), b"{\"profile\":").unwrap();
        set_aside_unreadable_setup_at(&root, false)
            .unwrap()
            .expect("the damaged progress is set aside");
        let recovered = saved_plan(&root).unwrap().unwrap();
        assert_eq!(recovered, on);
        InitialSetupAuthorization::capture_at(&root)
            .unwrap()
            .persist_at(&root, &recovered)
            .result
            .unwrap()
            .expect("the recovered plan is admitted");
    }

    // Permissions off and on is a new approval, including after a recovery.
    choose(&root, &off);
    choose(&root, &on);
    let approval = access_revision_at(&root).unwrap();
    let rows = permission_rows(state, approval);
    let fda = rows
        .iter()
        .find(|row| row.name == Access::FullDiskAccess.name())
        .unwrap();
    assert_eq!(
        fda.status, WAITING,
        "approval {approval} reused Not now from {old_approval}: {fda:?}"
    );
}

#[test]
fn permissions_reapproval_after_set_aside_does_not_reuse_an_old_not_now() {
    check_reapproval(true);
}

#[test]
fn permissions_reapproval_without_damage_does_not_reuse_an_old_not_now() {
    check_reapproval(false);
}
