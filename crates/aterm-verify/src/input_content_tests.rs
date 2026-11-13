// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn large_input_proof_survives_event_bounds_only_with_every_intact_part() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut ctx = Ctx::new(
        dir.path().into(),
        crate::Scope::workspace(),
        crate::EnvSnapshot::default(),
        dir.path().join("scratch"),
    );
    ctx.carry_context = Some("a".repeat(64));
    let proof = Proof::new(
        &ctx,
        "census",
        "large",
        (0..1000)
            .map(|i| ("file".into(), format!("crates/a/{i}.rs"), "b".repeat(64)))
            .collect(),
    )
    .unwrap();
    let path = dir.path().join("events");
    ctx.events = Some(crate::event_log::EventLog::open(&path).unwrap());
    let emitted = event(&ctx, &proof, false);
    assert!(emitted.lines().count() > 2);
    let rows: Vec<aterm_json::Value> = std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| aterm_json::from_str(line.strip_prefix("verify-event: ").unwrap()).unwrap())
        .collect();
    assert!(emitted.lines().all(|line| line.len() < 64 << 10));
    let mut collector = Collector::default();
    let accepted: Vec<_> = rows
        .iter()
        .filter_map(|row| collector.observe(row).unwrap())
        .collect();
    assert_eq!(accepted, vec![proof]);
    let mut missing = Collector::default();
    assert!(rows.iter().skip(1).any(|row| missing.observe(row).is_err()));
    let mut corrupted = rows;
    let original = corrupted[0]["data"].as_str().unwrap();
    let changed = original.replacen("\"file\"", "\"fyle\"", 1);
    assert_ne!(original, changed);
    corrupted[0]
        .as_object_mut()
        .unwrap()
        .insert("data".into(), aterm_json::Value::String(changed));
    let mut collector = Collector::default();
    assert!(corrupted.iter().any(|row| collector.observe(row).is_err()));
}

#[test]
fn source_proof_requires_equal_content_and_context_and_cannot_replace_cargo_validation() {
    let dir = aterm_tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("source.rs"), "one").unwrap();
    let mut ctx = Ctx::new(
        dir.path().into(),
        crate::Scope::workspace(),
        crate::EnvSnapshot::default(),
        dir.path().join("scratch"),
    );
    ctx.carry_context = Some("a".repeat(64));
    let input = aterm_census::inputs::file(dir.path(), "source.rs").unwrap();
    let proof = Proof::new(&ctx, "fmt", "package", vec![input]).unwrap();
    assert!(proof.reusable(&ctx));
    ctx.carry_context = Some("b".repeat(64));
    assert!(!proof.reusable(&ctx));
    ctx.carry_context = Some("a".repeat(64));
    std::fs::write(dir.path().join("source.rs"), "two").unwrap();
    assert!(!proof.reusable(&ctx));
}
