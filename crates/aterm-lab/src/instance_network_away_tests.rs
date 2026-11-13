// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn private_history_reaches_the_actual_away_boundary_and_refuses_reseeding() {
    let world = World::new(super::super::CONFIG_OFF).unwrap();
    // The CLI refuses even an unseeded existing world, before resolving or launching an app.
    assert_eq!(
        super::super::cmd_instance(&[
            "--world".into(),
            world.dir.to_string_lossy().into_owned(),
            "--network-away-in-ms".into(),
            "5000".into(),
        ])
        .unwrap_err()
        .code,
        2
    );
    assert_eq!(
        super::super::cmd_instance(&[
            "--aterm".into(),
            "/unavailable/release/aterm".into(),
            "--network-away-in-ms".into(),
            "5000".into(),
        ])
        .unwrap_err()
        .code,
        2
    );
    let now = 1_000_000;
    let fixture = prepare(&world, 5_000, now).unwrap();
    let script = std::fs::read_to_string(&fixture.facts).unwrap();
    let facts = aterm_netplace::facts::parse_script(&script).facts;
    let before =
        aterm_netplace::keeper::read_with(&world.state(), facts.clone(), fixture.due_ms - 1);
    let at = aterm_netplace::keeper::read_with(&world.state(), facts, fixture.due_ms);
    assert!(
        words::away_key_with_deadline(&before, fixture.due_ms - 1)
            .key
            .is_none()
    );
    assert!(
        words::away_key_with_deadline(&at, fixture.due_ms)
            .key
            .is_some()
    );
    let held = std::fs::read(record::dir(&world.state()).join(record::FILE)).unwrap();
    assert!(prepare(&world, 5_000, now).is_err());
    assert_eq!(
        std::fs::read(record::dir(&world.state()).join(record::FILE)).unwrap(),
        held
    );
    assert!(delay("0").is_err());
    assert!(delay(&(words::AWAY_ENTRY_AFTER_MS + 1).to_string()).is_err());
}
