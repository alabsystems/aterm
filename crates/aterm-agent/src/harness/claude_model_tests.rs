// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn family_versions() {
    assert_eq!(
        family_version("claude-opus-5-5"),
        Some(("opus".into(), (5, 5)))
    );
    assert_eq!(
        family_version("claude-opus-5"),
        Some(("opus".into(), (5, 0)))
    );
    assert_eq!(
        family_version("claude-fable-5-1[1m]"),
        Some(("fable".into(), (5, 1)))
    );
    assert_eq!(
        family_version("claude-haiku-4-5-20251001"),
        Some(("haiku".into(), (4, 5)))
    );
    assert_eq!(family_version("claude-opus"), None);
    assert_eq!(family_version("claude-opus-x"), None);
    assert_eq!(family_version("gpt-5"), None);
}

#[test]
fn a_result_row_names_its_kind_its_model_and_its_effort() {
    let row = |text: &str| format!("<local-command-stdout>{text}</local-command-stdout>");
    assert_eq!(
        model_result_of(&row(
            "Set model to `Sonnet 5` for this session only with `high` effort"
        )),
        Some(ModelResult {
            kind: ModelResultKind::Set,
            display: Some("Sonnet 5".into()),
            effort: Some("high".into()),
        })
    );
    let coloured = "\u{1b}[38;2;255;106;0m\u{21AF}\u{1b}[39m Fast mode ON \u{00B7} model set to `Opus 5.5` \u{00B7} $30/$150 per Mtok";
    assert_eq!(
        strip_sgr(coloured),
        "\u{21AF} Fast mode ON \u{00B7} model set to `Opus 5.5` \u{00B7} $30/$150 per Mtok"
    );
    assert_eq!(
        model_result_of(&row(coloured)),
        Some(ModelResult {
            kind: ModelResultKind::FastOn,
            display: Some("Opus 5.5".into()),
            effort: None,
        })
    );
    assert_eq!(
        model_result_of("why did it say Set model to `claude-fable-5-1`?"),
        None
    );
}
