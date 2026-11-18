// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn shaping_history_never_promotes_plain_lint_or_an_unseen_compiler_width() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join(aterm_capacity::book::VERIFY_RECORD);
    let plain = dir.path().join("plain.json");
    let scope = crate::Scope::workspace();
    let lint = crate::plan::declared(&scope)
        .into_iter()
        .find(|spec| spec.id == StageId::Tippy)
        .unwrap();
    let mut key = Key::of(StageId::Tippy, &scope);
    key.checkout = "/fixture".into();
    let mut ordinary = Records::default();
    ordinary.push(
        Key {
            stage: "targo tippy".into(),
            variant: Some("--all-targets".into()),
            ..key.clone()
        },
        4 << 30,
        1,
    );
    ordinary.save(&plain).unwrap();
    aterm_capacity::book::keep_shaping_unit(
        &path,
        crate::shape::unit_key(
            "/fixture",
            &crate::unitgraph::UnitName {
                package: "unit".into(),
                kind: "unit lib".into(),
            },
        ),
        24 << 30,
        2,
    )
    .unwrap();
    let book =
        Book::at(Some(path.clone()), Some(128 << 30), Some(120 << 30)).with_plain(Some(plain));
    let need = book.need(&lint, &key, Some(1));
    assert!(matches!(need.how, How::Plain(_)));
    let claim = crate::compile_width::lint_claim(0, &need, book.physical, book.budget());
    assert!(
        !claim.measured && claim.alone,
        "weak and plain rows cannot authenticate the lint peak"
    );

    let measured = crate::compile_width::record_key(key.clone(), Some(1));
    aterm_capacity::book::keep_stage(&path, measured, 24 << 30, 3, true).unwrap();
    let need = book.need(&lint, &key, Some(1));
    assert!(matches!(need.how, How::Learned(_)));
    let claim = crate::compile_width::lint_claim(0, &need, book.physical, book.budget());
    assert!(claim.measured && !claim.alone);
    let unseen = book.need(&lint, &key, Some(2));
    let claim = crate::compile_width::lint_claim(0, &unseen, book.physical, book.budget());
    assert!(
        !claim.measured && claim.alone,
        "retention cannot erase the compiler-width key"
    );
}
