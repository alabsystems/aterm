// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
use super::*;

#[test]
fn foreign_and_paused_app_scopes_refuse_before_ticket_authentication() {
    let world = NativeSourceProofWorld::new().unwrap();
    let app = account_work::Lifetime::default();
    let foreign = account_work::Lifetime::default();
    let foreign_scope = foreign.enter().unwrap();
    let frame = format!("{SOURCE_PROOF_TASK}not-a-ticket");
    assert_eq!(
        SourceProofLaunch::admit(world.env(), &frame, &app, &foreign_scope, "s-proof")
            .err()
            .as_deref(),
        Some("source proof admission unavailable")
    );
    let scope = app.enter().unwrap();
    let _pause = app.pause().unwrap();
    assert_eq!(
        SourceProofLaunch::admit(world.env(), &frame, &app, &scope, "s-proof")
            .err()
            .as_deref(),
        Some("source proof admission unavailable")
    );
    assert!(!world.directory().read_dir().unwrap().any(|entry| {
        entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with(".claimed")
    }));
}
