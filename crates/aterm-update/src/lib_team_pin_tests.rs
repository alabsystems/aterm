// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

#[test]
fn the_runtime_team_requirement_can_only_tighten() {
    assert!(
        !super::PINNED_TEAM_ID.is_empty(),
        "Tier APPLE is armed; an empty pin changes this test's meaning"
    );
    assert_eq!(
        super::effective_team_id(),
        super::PINNED_TEAM_ID,
        "the pin decides"
    );

    super::set_required_team_id(None);
    super::set_required_team_id(Some("   "));
    super::set_required_team_id(Some("ZZZZZ99999"));
    assert_eq!(
        super::effective_team_id(),
        super::PINNED_TEAM_ID,
        "a settings file can neither relax nor replace a compiled pin"
    );
}
