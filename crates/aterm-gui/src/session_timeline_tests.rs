// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_screen_wall_detail_keeps_the_limit_episode_start() {
    let mut timeline = SessionTimeline::default();
    let stamp = || AgentStamp {
        generation: crate::control::ScreenGen { epoch: 1, seq: 1 },
        fp: 1,
    };
    timeline.publish_agent(
        "wall:usage-weekly",
        None,
        None,
        Some(aterm_phase::Program::Claude),
        stamp(),
    );
    timeline.agent.screen_word_unix_ms = 17;
    timeline.publish_agent(
        "wall:usage-weekly",
        Some("a later countdown".into()),
        None,
        Some(aterm_phase::Program::Claude),
        stamp(),
    );
    assert_eq!(
        timeline.agent.screen_word_unix_ms, 17,
        "a changed detail cannot turn a cleared wall into a new rejection"
    );
    timeline.publish_agent(
        "idle",
        None,
        None,
        Some(aterm_phase::Program::Claude),
        stamp(),
    );
    timeline.publish_agent(
        "wall:usage-weekly",
        None,
        None,
        Some(aterm_phase::Program::Claude),
        stamp(),
    );
    assert!(
        timeline.agent.screen_word_unix_ms > 17,
        "a later limit episode has its own start"
    );
}

#[test]
fn native_exec_claim_transfer_keeps_identity_and_rejects_stale_authority() {
    let proof = crate::self_claim::Proven {
        pid: 4242,
        start: "birth".into(),
    };
    let mut meta = SessionMeta::default();
    meta.set_supervisor(
        Some(SupervisorClaim {
            holder: "codex-alab@4242".into(),
            conn: Some(11),
            expires_us: None,
            proven: Some(proof.clone()),
            scoped: None,
        }),
        100,
    );
    let old = meta.supervisor_authority(100);
    let moved = meta
        .transfer_native_exec_claim(&old, 12, 100)
        .expect("same native claim");
    assert_eq!(moved.identity(), Some(&proof));
    assert_eq!(moved.connection(), Some(12));
    assert_eq!(moved.scopes(), old.scopes());
    assert!(
        !meta
            .claimant_conn
            .iter()
            .any(|(connection, _)| *connection == 11)
    );
    assert!(meta.transfer_native_exec_claim(&old, 13, 100).is_none());
    meta.set_supervisor(None, 100);
    assert!(meta.transfer_native_exec_claim(&moved, 13, 100).is_none());
}

#[test]
fn passive_relaunch_continuity_rejects_spoofed_holder_and_expired_foreign_aba() {
    let mut meta = SessionMeta::default();
    let original = meta.supervisor_authority(100);
    meta.set_supervisor(None, 100);
    let benign = meta.supervisor_authority(100);
    assert_ne!(original.revision(), benign.revision());
    assert!(original.same_builtin_relaunch_continuity(&benign));
    // Identical attribution text is not authenticated built-in custody.
    meta.set_supervisor(
        Some(SupervisorClaim {
            holder: crate::harness_host::holder(),
            conn: Some(71),
            expires_us: Some(101),
            proven: None,
            scoped: None,
        }),
        100,
    );
    assert!(!original.same_builtin_relaunch_continuity(&meta.supervisor_authority(100)));
    // Expiry cannot hide the intervening foreign acquisition, even when the
    // next writer observes an already absent live claim.
    meta.set_supervisor(None, 102);
    assert!(!original.same_builtin_relaunch_continuity(&meta.supervisor_authority(102)));
    assert_eq!(meta.builtin_relaunch_epoch.test_value(), Some(2));
}

#[test]
fn passive_relaunch_continuity_exhaustion_never_reuses_a_generation() {
    let mut meta = SessionMeta::default();
    let original = meta.supervisor_authority(100);
    assert!(original.same_builtin_relaunch_continuity(&original));
    meta.builtin_relaunch_epoch = RelaunchEpoch::for_test(u64::MAX - 1);
    meta.set_supervisor(
        Some(SupervisorClaim {
            holder: "foreign".into(),
            conn: Some(71),
            expires_us: None,
            proven: None,
            scoped: None,
        }),
        100,
    );
    assert_eq!(meta.builtin_relaunch_epoch.test_value(), Some(u64::MAX));
    meta.set_supervisor(None, 100);
    assert_eq!(meta.builtin_relaunch_epoch.test_value(), None);
    let exhausted = meta.supervisor_authority(100);
    assert!(!original.same_builtin_relaunch_continuity(&exhausted));
    assert!(!exhausted.same_builtin_relaunch_continuity(&exhausted));
    // A benign claim revision cannot reopen the exhausted generation space.
    meta.set_supervisor(None, 101);
    assert_eq!(meta.builtin_relaunch_epoch.test_value(), None);
    assert!(!exhausted.same_builtin_relaunch_continuity(&meta.supervisor_authority(101)));
}

/// One capture preserves a claim's identity, binding and revision, and assigns every duty
/// exactly once. Legacy foreign claims and v2 retain full ownership; v3 takes its subset.
/// An expired claim is absent, and reading either snapshot changes no revision.
#[test]
fn supervisor_authority_captures_partial_full_and_expired_ownership() {
    use aterm_agent::harness::contract::{Scope, Words, read};
    let host = "aterm-harness@7";
    let proof = crate::self_claim::Proven {
        pid: 4242,
        start: "birth".to_string(),
    };
    let Words::Scoped(claim) = read("grok-alab@4242", &["contract=3", "scope=upgrade"]) else {
        panic!("the roster admits an upgrade claim");
    };
    let mut meta = SessionMeta {
        supervisor_revision: 7,
        ..SessionMeta::default()
    };
    let empty = meta.supervisor_authority(10);
    assert_eq!(empty.holder(), None);
    assert_eq!(empty.contract(), None);
    assert_eq!(empty.identity(), None);
    assert_eq!(empty.connection(), None);
    assert_eq!(empty.scopes(), None);
    assert_eq!(empty.revision(), 7);
    assert_eq!(
        empty.wire_fields(),
        "supervisor_contract=- supervisor_scope=- supervisor_revision=7"
    );
    for duty in Scope::ALL {
        assert!(!empty.external_takes(host, duty));
        assert!(empty.builtin_keeps(host, duty));
    }

    meta.supervisor = Some(SupervisorClaim {
        holder: "grok-alab@4242".to_string(),
        conn: Some(4),
        expires_us: None,
        proven: None,
        scoped: Some(crate::harness_claim::Scoped {
            proven: proof.clone(),
            claim,
        }),
    });
    let partial = meta.supervisor_authority(10);
    assert_eq!(partial.holder(), Some("grok-alab@4242"));
    assert_eq!(partial.contract(), Some(3));
    assert_eq!(partial.identity(), Some(&proof));
    assert_eq!(partial.connection(), Some(4));
    assert_eq!(
        partial.scopes().map(|s| s.words()).as_deref(),
        Some("upgrade")
    );
    assert_eq!(
        partial.wire_fields(),
        "supervisor_contract=3 supervisor_scope=upgrade supervisor_revision=7"
    );
    for duty in Scope::ALL {
        assert_eq!(partial.external_takes(host, duty), duty == Scope::Upgrade);
        assert_eq!(partial.builtin_keeps(host, duty), duty != Scope::Upgrade);
    }

    for (holder, proven, contract) in [
        ("foreign", None, None),
        ("codex-alab@4242", Some(proof.clone()), Some(2)),
        (host, None, None),
    ] {
        meta.supervisor = Some(SupervisorClaim {
            holder: holder.to_string(),
            conn: Some(5),
            expires_us: Some(20),
            proven,
            scoped: None,
        });
        let live = meta.supervisor_authority(19);
        assert_eq!(live.contract(), contract, "{holder}");
        assert_eq!(live.scopes(), None, "implicit full scope");
        assert!(
            live.wire_fields()
                .contains("supervisor_scope=relaunch,upgrade,turn-end,approvals,questions,walls")
        );
        for duty in Scope::ALL {
            assert_eq!(live.external_takes(host, duty), holder != host);
            assert_eq!(live.builtin_keeps(host, duty), holder == host);
        }
        let expired = meta.supervisor_authority(20);
        assert_eq!(expired.holder(), None);
        assert_eq!(expired.contract(), None);
        assert_eq!(expired.identity(), None);
        assert_eq!(expired.connection(), None);
        assert_eq!(expired.scopes(), None);
        assert_eq!(expired.revision(), 7);
        assert!(
            Scope::ALL
                .into_iter()
                .all(|duty| expired.builtin_keeps(host, duty))
        );
    }
    assert_eq!(meta.supervisor_revision, 7, "capture is read-only");
    assert_eq!(
        partial.contract(),
        Some(3),
        "an earlier capture stays unchanged"
    );
    assert_eq!(partial.connection(), Some(4));
    assert!(partial.external_takes(host, Scope::Upgrade));
}

#[test]
fn metadata_sanitizer_is_single_line_bidi_safe_and_grapheme_bounded() {
    let hostile = "  alpha\nbe\u{0085}ta\u{202e}spoof\u{2066}\u{feff}  ";
    assert!(metadata_has_forbidden_formatting(hostile));
    assert_eq!(
        sanitize_metadata_value("title", hostile).as_deref(),
        Some("alphabetaspoof")
    );
    for invisible in [
        '\u{00ad}',
        '\u{034f}',
        '\u{180b}',
        '\u{115f}',
        '\u{3164}',
        '\u{ffa0}',
        '\u{1bca0}',
        '\u{1d173}',
        '\u{e0001}',
        '\u{fdd0}',
        '\u{fffe}',
        '\u{1ffff}',
        '\u{fff9}',
        '\u{fffb}',
    ] {
        assert!(
            metadata_has_forbidden_formatting(&format!("a{invisible}b")),
            "spoof-relevant default-ignorable U+{:04X} is rejected",
            u32::from(invisible)
        );
    }

    let family = "👨‍👩‍👧‍👦";
    assert!(!metadata_has_forbidden_formatting(family));
    assert_eq!(
        sanitize_metadata_value("icon", family).as_deref(),
        Some(family)
    );
    for legitimate in ["a\u{200c}b", "✈\u{fe0f}", "漢\u{e0100}"] {
        assert!(
            !metadata_has_forbidden_formatting(legitimate),
            "joiners/standard variation selectors remain available: {legitimate:?}"
        );
        assert_eq!(
            sanitize_metadata_value("title", legitimate).as_deref(),
            Some(legitimate)
        );
    }

    let cluster = "e\u{301}";
    let expected = cluster.repeat(META_TITLE_MAX / cluster.len());
    let over_cap = format!("{expected}z");
    let sanitized = sanitize_metadata_value("title", &over_cap).expect("non-empty");
    assert_eq!(sanitized, expected);
    assert_eq!(sanitized.len(), META_TITLE_MAX);
}

#[test]
fn session_meta_defensively_canonicalizes_internal_callers() {
    let mut meta = SessionMeta::default();
    assert_eq!(
        meta.set("title", Some(" safe\n\u{202e}title ".to_string())),
        Some(true)
    );
    assert_eq!(meta.user_title.as_deref(), Some("safetitle"));

    meta.description = Some("one\u{2029}two".to_string());
    meta.icon = Some("\u{2066}🚀\u{2069}".to_string());
    let canonical = meta.sanitized();
    assert_eq!(canonical.description.as_deref(), Some("onetwo"));
    assert_eq!(canonical.icon.as_deref(), Some("🚀"));
}

#[test]
fn cwd_change_records_only_actual_changes_and_clears_to_dash() {
    let mut tl = SessionTimeline::default();
    tl.record_cwd_change(Some("/a"));
    tl.record_cwd_change(Some("/a")); // reprompt: no new event
    tl.record_cwd_change(Some("/b b")); // space -> pct-encoded
    tl.record_cwd_change(None); // OSC 7 cleared
    tl.record_cwd_change(None); // still cleared: no new event
    let rows: Vec<TimelineEvent> = tl.since(None).collect();
    let got: Vec<(&str, &str)> = rows.iter().map(|e| (e.kind, &*e.payload)).collect();
    assert_eq!(
        got,
        vec![
            ("cwd-change", "cwd=/a"),
            ("cwd-change", "cwd=/b%20b"),
            ("cwd-change", "cwd=-"),
        ]
    );
}

/// The clamp, and a row the view prints: a long payload is cut on a char boundary.
#[test]
fn payload_clamps_on_a_char_boundary() {
    let long = "é".repeat(400); // 800 bytes
    let clamped = clamp_payload(long.clone());
    assert!(clamped.len() <= MAX_PAYLOAD && clamped.len() >= MAX_PAYLOAD - 5);
    assert!(clamped.chars().all(|c| c == 'é'), "boundary-safe clamp");
    let mut tl = SessionTimeline::default();
    tl.record_raw_meta_change(long);
    let e = tl.since(None).next().unwrap();
    assert!(e.payload.len() <= MAX_PAYLOAD);
    assert!(e.payload.chars().all(|c| c == 'é'), "boundary-safe row");
}

/// The clamp, and a row the view prints: no `%XX` escape is bisected.
#[test]
fn payload_clamp_never_bisects_a_pct_escape() {
    let whole = |stored: &str, pad: usize| {
        let b = stored.as_bytes();
        for (i, &c) in b.iter().enumerate() {
            if c == b'%' {
                assert!(
                    i + 2 < b.len() && b[i + 1].is_ascii_hexdigit() && b[i + 2].is_ascii_hexdigit(),
                    "dangling escape at byte {i} of {stored:?} (pad {pad})"
                );
            }
        }
    };
    for pad in 0..3usize {
        let encoded = crate::control::pct_encode(&"中".repeat(MAX_PAYLOAD)); // %E4%B8%AD…
        let payload = format!("{}{encoded}", "x".repeat(pad));
        let clamped = clamp_payload(payload.clone());
        assert!(clamped.len() <= MAX_PAYLOAD, "cap holds (pad {pad})");
        whole(&clamped, pad);
        let mut tl = SessionTimeline::default();
        tl.record_raw_meta_change(payload);
        let stored = tl.since(None).next().unwrap().payload;
        assert!(stored.len() <= MAX_PAYLOAD, "cap holds (pad {pad})");
        whole(&stored, pad);
    }
}

#[test]
fn meta_set_reports_change_caps_and_unknown_fields() {
    let mut m = SessionMeta::default();
    assert!(!m.any_set());
    assert_eq!(m.set("title", Some("build agent".into())), Some(true));
    assert_eq!(m.get("title"), Some("build agent"));
    assert!(m.any_set());
    assert_eq!(m.set("title", Some("build agent".into())), Some(false));
    assert_eq!(m.set("title", None), Some(true));
    assert_eq!(m.set("title", None), Some(false));
    assert!(!m.any_set());
    assert_eq!(m.set("colour", Some("red".into())), None);
    assert_eq!(m.get("colour"), None);
    assert_eq!(m.set("role", Some("reviewer".into())), Some(true));
    assert_eq!(m.get("role"), Some("reviewer"));
    assert!(m.any_set());
    assert_eq!(
        m.set("attention", Some("needs human: approval".into())),
        Some(true)
    );
    assert_eq!(m.get("attention"), Some("needs human: approval"));
    assert_eq!(m.set("role", None), Some(true));
    assert_eq!(m.set("attention", None), Some(true));
    assert!(!m.any_set());
}

/// NEGATIVE CONTROL: when the awake clock is unknown (`0`) or still inside `WAKE_GRACE_S`,
/// `owner_columns_with_awake` keeps `upgrade=pending/...`; once `now >= awake + WAKE_GRACE_S`,
/// the same stored row reads `upgrade=stalled/.../unwatched:3h/1h`.
#[test]
fn the_upgrade_column_reads_the_awake_clock_as_it_stands_now() {
    use aterm_agent::harness::upgrade::Phase;
    use aterm_agent::harness::upgrade_drive::{Row, WAKE_GRACE_S};
    let now = 1_800_000_000_u64;
    let row = Row {
        tab: "s-a".into(),
        from: "2.1.281".into(),
        to: "2.1.282".into(),
        phase: Phase::Pending,
        behind_since: now - 3_600,
        watch_at: now - 2 * 3_600,
        looked_at: now - 3 * 3_600,
        awake_since: 0,
        ..Row::default()
    };
    assert_eq!(row.column(now), "pending/2.1.282/-/1h", "as stored");
    let mut tl = SessionTimeline::default();
    assert!(tl.set_upgrade(Some(row)));
    for awake in [0, now - WAKE_GRACE_S + 1] {
        let (_, cols) = tl.owner_columns_with_awake(false, now, awake);
        assert!(
            cols.ends_with(" upgrade=pending/2.1.282/-/1h"),
            "awake={awake}: {cols}"
        );
    }
    for awake in [now - WAKE_GRACE_S, now - 4 * 3_600] {
        let (_, cols) = tl.owner_columns_with_awake(false, now, awake);
        assert!(
            cols.ends_with(" upgrade=stalled/2.1.282/unwatched:3h/1h"),
            "awake={awake}: {cols}"
        );
    }
    let (_, live_cols) = tl.owner_columns(false, now);
    assert!(live_cols.contains(" upgrade="), "{live_cols}");
}
