// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

#[test]
fn focused_pacing_builds_no_conformance_matrices() {
    let mut context = super::tests::ctx(
        &[crate::checks::Check::Measure],
        crate::scope::Scope::workspace(),
    );
    context.cursor_pacing = true;
    let stages = super::plan(&context);
    assert_eq!(
        stages.iter().map(|stage| stage.id).collect::<Vec<_>>(),
        [super::StageId::GuiSmoke]
    );
}
