// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;
use crate::{Profile, profiles, selector::DispatchedSequence};

fn osc(major: u32, params: &[&str]) -> DispatchedSequence {
    DispatchedSequence::osc(major, params.iter().map(|&s| s.to_owned()))
}

fn csi(major: Option<u32>, final_byte: char, params: &[&str]) -> DispatchedSequence {
    DispatchedSequence::csi(major, final_byte, params.iter().map(|&s| s.to_owned()))
}

/// One sequence from one origin under one built-in profile:
/// `(what, profile, sequence, origin, response, rate-limit id)`.
type ProfileRow = (
    &'static str,
    fn() -> crate::Policy,
    DispatchedSequence,
    OriginTag,
    Response,
    Option<&'static str>,
);

#[test]
#[rustfmt::skip]
fn builtin_profile_decisions() {
    use {OriginTag as O, Response as R};
    let permissive: fn() -> crate::Policy = profiles::permissive;
    let standard: fn() -> crate::Policy = profiles::standard;
    let hardened: fn() -> crate::Policy = profiles::hardened;
    let clip_set = || osc(52, &["c", "SGVsbG8="]);
    let clip_query = || osc(52, &["c", "?"]);
    let palette_query = || osc(4, &["3", "?"]);
    let modal = || DispatchedSequence::dcs("2000p");
    let rows: Vec<ProfileRow> = vec![
        ("permissive unmatched", permissive, osc(52, &["c", "SGk="]), O::Pty, R::Execute, None),
        ("permissive window op", permissive, csi(Some(20), 't', &[]), O::Pty, R::Execute, None),
        ("standard clip set, user", standard, clip_set(), O::User, R::Ask,
         Some("clipboard")),
        ("standard clip set, pty", standard, clip_set(), O::Pty, R::Execute, None),
        ("standard clip query, host", standard, clip_query(), O::Host, R::Drop, None),
        ("standard palette query, pty-safe", standard, palette_query(), O::PtySafe, R::Execute,
         None),
        ("standard palette query, pty", standard, palette_query(), O::Pty, R::Execute,
         None),
        ("standard window op, pty", standard, csi(Some(20), 't', &[]), O::Pty, R::Execute,
         None),
        ("standard notification, user", standard, osc(9, &["build done"]), O::User, R::Warn,
         Some("notifications")),
        ("standard dcs 3000p, host", standard, DispatchedSequence::dcs("3000p"), O::Host,
         R::Execute, None),
        ("hardened clip set, user", hardened, clip_set(), O::User, R::Drop, None),
        ("hardened clip set, host", hardened, clip_set(), O::Host, R::Drop, None),
        ("hardened clip query, host", hardened, clip_query(), O::Host, R::Drop, None),
        ("hardened palette query, pty", hardened, palette_query(), O::Pty, R::Drop, None),
        ("hardened palette query, config", hardened, palette_query(), O::ConfigFile, R::Execute,
         None),
        ("hardened modal activation, host", hardened, modal(), O::Host, R::Execute, None),
        ("hardened modal activation, pty", hardened, modal(), O::Pty, R::Drop, None),
        ("hardened unknown, network", hardened, osc(1337, &["leet"]), O::NetworkUntrusted,
         R::Drop, None),
        ("hardened unknown, host", hardened, osc(1337, &["leet"]), O::Host, R::Execute,
         None),
    ];
    for (what, profile, sequence, origin, response, rate_limit) in rows {
        let d = PolicyEngine::new(profile()).evaluate(&sequence, origin);
        assert_eq!(d.response, response, "{what}");
        if rate_limit.is_some() {
            assert_eq!(d.rate_limit.as_deref(), rate_limit, "{what}: rate limit");
        }
    }
}

#[test]
fn malformed_rule_selector_is_skipped() {
    let mut p = profiles::standard();
    p.rules.insert(
        0,
        crate::Rule {
            sequence: "ZZZ garbage".to_owned(),
            origin_min: OriginTag::Pty,
            response: Response::Drop,
            rate_limit: None,
            prompt_id: None,
        },
    );
    let eng = PolicyEngine::new(p);
    let d = eng.evaluate(&osc(52, &["c", "SGVsbG8="]), OriginTag::User);
    assert_eq!(d.response, Response::Ask);
}

#[test]
fn origin_gate_failure_does_not_short_circuit() {
    let p = Policy {
        schema_version: crate::SCHEMA_VERSION,
        profile: Profile::Standard,
        defaults: crate::Defaults {
            unmatched: Response::Drop,
            shell_integration_require_nonce: false,
        },
        rules: vec![
            crate::Rule {
                sequence: "OSC 9".to_owned(),
                origin_min: OriginTag::Host,
                response: Response::Drop,
                rate_limit: None,
                prompt_id: None,
            },
            crate::Rule {
                sequence: "OSC 9".to_owned(),
                origin_min: OriginTag::NetworkUntrusted,
                response: Response::Warn,
                rate_limit: None,
                prompt_id: None,
            },
        ],
        rate_limits: vec![],
    };
    let eng = PolicyEngine::new(p);
    let d = eng.evaluate(&osc(9, &["ping"]), OriginTag::Pty);
    assert_eq!(d.response, Response::Warn);
    assert_eq!(d.matched_rule, Some(1));
}

#[test]
fn rate_limit_config_matches_last_wins_duplicate_id() {
    let mut p = profiles::standard();
    p.rate_limits = vec![
        RateLimit {
            id: "dup".to_owned(),
            capacity_bytes: 100,
            refill_per_second: 1,
            per_sequence_max: 10,
        },
        RateLimit {
            id: "dup".to_owned(),
            capacity_bytes: 250,
            refill_per_second: 5,
            per_sequence_max: 50,
        },
    ];
    let eng = PolicyEngine::new(p);
    let cfg = eng.rate_limit_config("dup").expect("declared");
    assert_eq!(cfg.capacity_bytes, 250);
    assert_eq!(cfg.refill_per_second, 5);
    assert_eq!(cfg.per_sequence_max, 50);
    assert!(eng.rate_limit_config("missing").is_none());
}
