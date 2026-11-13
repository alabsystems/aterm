// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

struct Adapter {
    authority: Arc<Mutex<BuiltinAuthority>>,
    typed: Vec<(Scope, u64, Vec<String>)>,
    legacy: Vec<Vec<String>>,
}

impl Ctl for Adapter {
    fn call(&mut self, args: &[&str]) -> Result<CtlReply, String> {
        self.legacy
            .push(args.iter().map(|word| word.to_string()).collect());
        Ok(CtlReply {
            code: 0,
            stdout: if args.last() == Some(&"meta") {
                "OK supervisor=- autopilot=cruise\n"
            } else {
                "OK\n"
            }
            .to_string(),
            stderr: String::new(),
        })
    }
    fn builtin_authority(&mut self, _: &str) -> BuiltinAuthority {
        *self.authority.lock().unwrap()
    }
    fn call_for_duty(
        &mut self,
        duty: Scope,
        revision: u64,
        args: &[&str],
    ) -> Option<Result<CtlReply, String>> {
        let current = *self.authority.lock().unwrap();
        self.typed.push((
            duty,
            revision,
            args.iter().map(|word| word.to_string()).collect(),
        ));
        Some(Ok(CtlReply {
            code: i32::from(current.revision() != Some(revision) || !current.keeps(duty)),
            stdout: "OK\n".to_string(),
            stderr: String::new(),
        }))
    }
}

#[test]
fn actual_session_retains_complement_and_pins_semantic_revision_without_legacy_fallback() {
    let authority = Arc::new(Mutex::new(BuiltinAuthority::complement(
        41,
        [Scope::Upgrade],
    )));
    let mut adapter = Adapter {
        authority: Arc::clone(&authority),
        typed: Vec::new(),
        legacy: Vec::new(),
    };
    let mut session = Session::new(&mut adapter, Some("@s-duty".to_string()));
    session.set_builtin_duties(true);
    assert!(session.refresh_builtin_authority());
    assert!(
        !session.call(&["key", "1"]).unwrap().ok(),
        "unlabelled input is refused"
    );
    session.with_duty(Scope::Approvals, |session| {
        assert!(session.call(&["key", "1"]).unwrap().ok());
        *authority.lock().unwrap() = BuiltinAuthority::complement(42, [Scope::Upgrade]);
        assert!(
            !session.call(&["key", "enter"]).unwrap().ok(),
            "later unsent command keeps old revision and is refused"
        );
    });
    *authority.lock().unwrap() = BuiltinAuthority::complement(43, [Scope::Questions]);
    session.with_duty(Scope::Questions, |session| {
        assert!(!session.call(&["send", "answer"]).unwrap().ok());
    });
    drop(session);
    assert_eq!(adapter.typed.len(), 2);
    assert_eq!(adapter.typed[0].0, Scope::Approvals);
    assert_eq!(adapter.typed[0].1, 41);
    assert_eq!(adapter.typed[1].1, 41);
    assert!(
        adapter.legacy.is_empty(),
        "typed refusal never retries via ordinary adapter"
    );
}

#[test]
fn unclaimed_host_keeps_its_visibility_lease_and_revision_bound_input() {
    let authority = Arc::new(Mutex::new(BuiltinAuthority::complement(0, [])));
    let mut adapter = Adapter {
        authority: Arc::clone(&authority),
        typed: Vec::new(),
        legacy: Vec::new(),
    };
    let mut session = Session::new(&mut adapter, Some("@s-duty".to_string()));
    session.set_builtin_duties(true);
    session.claim_at_naming();
    assert!(matches!(session.claim, Claim::Held { .. }));
    *authority.lock().unwrap() = BuiltinAuthority::complement(1, []);
    session.with_duty(Scope::Approvals, |session| {
        assert!(matches!(session.claim, Claim::Held { .. }));
        assert!(session.call(&["key", "1"]).unwrap().ok());
    });
    *authority.lock().unwrap() = BuiltinAuthority::complement(2, [Scope::Upgrade]);
    session.refresh_builtin_authority();
    session.renew_claim_if_due();
    assert!(matches!(session.claim, Claim::Complement));
    *authority.lock().unwrap() = BuiltinAuthority::complement(3, []);
    session.refresh_builtin_authority();
    session.renew_claim_if_due();
    assert!(matches!(session.claim, Claim::Held { .. }));
    session.give_back_claim();
    session.refresh_builtin_authority();
    session.renew_claim_if_due();
    assert!(matches!(session.claim, Claim::GivenBack { .. }));
    drop(session);
    assert_eq!(adapter.typed.len(), 1);
    assert_eq!(adapter.typed[0].0, Scope::Approvals);
    assert_eq!(adapter.typed[0].1, 1);
    assert_eq!(
        adapter
            .legacy
            .iter()
            .map(|args| args[2].as_str())
            .collect::<Vec<_>>(),
        ["set", "set", "unset"],
        "only naming, the foreign owner's departure, and Off change the lease"
    );
}

#[test]
fn ordinary_adapter_and_v2_exclusivity_remain_distinct() {
    let authority = Arc::new(Mutex::new(BuiltinAuthority::Exclusive));
    let mut adapter = Adapter {
        authority,
        typed: Vec::new(),
        legacy: Vec::new(),
    };
    let mut ordinary = Session::new(&mut adapter, None);
    ordinary.with_duty(Scope::Questions, |session| {
        assert!(session.call(&["key", "1"]).unwrap().ok())
    });
    drop(ordinary);
    assert_eq!(adapter.legacy.len(), 1);
    let mut hosted = Session::new(&mut adapter, Some("@s-duty".to_string()));
    hosted.set_builtin_duties(true);
    hosted.claim = Claim::Behind {
        holder: "codex-alab@4242".to_string(),
        tried: hosted.fx.mono(),
    };
    hosted.with_duty(Scope::Approvals, |session| {
        assert!(!session.call(&["key", "1"]).unwrap().ok())
    });
    drop(hosted);
    assert!(adapter.typed.is_empty());
    assert_eq!(adapter.legacy.len(), 1);
}

#[test]
fn usage_resume_keeps_its_explicit_turn_end_origin_for_all_three_ownership_cases() {
    let action = super::super::TurnEndAction::Type {
        text: "continue".to_string(),
        rule_id: super::super::RULE_LIMIT_RESUME,
    };
    let scope = action_scope(&action, true, false);
    assert_eq!(scope, Scope::TurnEnd);
    for (taken, allowed) in [
        (vec![], true),
        (vec![Scope::Walls], true),
        (vec![Scope::TurnEnd], false),
    ] {
        let authority = Arc::new(Mutex::new(BuiltinAuthority::complement(41, taken)));
        let mut adapter = Adapter {
            authority,
            typed: Vec::new(),
            legacy: Vec::new(),
        };
        let mut session = Session::new(&mut adapter, Some("@s-duty".to_string()));
        session.set_builtin_duties(true);
        session.with_duty(scope, |session| {
            assert_eq!(session.call(&["turn", "continue"]).unwrap().ok(), allowed)
        });
        drop(session);
        assert!(
            adapter
                .legacy
                .iter()
                .all(|args| args.get(1).map(String::as_str) == Some("meta")),
            "only the visibility lease uses the ordinary adapter; input stays typed"
        );
        assert_eq!(adapter.typed.len(), usize::from(allowed));
    }
}

#[test]
fn complementary_off_park_never_unsets_foreign_claim_and_ordinary_remedy_is_preserved() {
    let authority = Arc::new(Mutex::new(BuiltinAuthority::complement(41, [])));
    let mut adapter = Adapter {
        authority: Arc::clone(&authority),
        typed: Vec::new(),
        legacy: Vec::new(),
    };
    let mut session = Session::new(&mut adapter, Some("@s-duty".to_string()));
    session.set_builtin_duties(true);
    assert!(
        !session.remedies_withheld(),
        "ordinary no-foreign session retains outside remedy policy"
    );
    *authority.lock().unwrap() = BuiltinAuthority::complement(42, [Scope::Upgrade]);
    assert!(
        session.remedies_withheld(),
        "foreign coordination conservatively stands down"
    );
    session.give_back_claim();
    assert!(matches!(session.claim, Claim::GivenBack { .. }));
    session.refresh_builtin_authority();
    assert!(
        matches!(session.claim, Claim::GivenBack { .. }),
        "a read cannot unpark the loop"
    );
    session.retake_claim(&mut super::super::Alone);
    assert!(matches!(session.claim, Claim::Complement));
    *authority.lock().unwrap() = BuiltinAuthority::Unknown;
    assert!(
        session.remedies_withheld(),
        "unknown private authority never grants a native act"
    );
    drop(session);
    assert!(
        adapter.legacy.is_empty(),
        "parking the complement never sends meta unset supervisor"
    );
}

#[test]
fn direct_needs_cleanup_and_timer_retry_retain_explicit_turn_end_metadata_duty() {
    let authority = Arc::new(Mutex::new(BuiltinAuthority::complement(41, [])));
    let mut adapter = Adapter {
        authority,
        typed: Vec::new(),
        legacy: Vec::new(),
    };
    let mut session = Session::new(&mut adapter, Some("@s-duty".to_string()));
    session.set_builtin_duties(true);
    session.needs_told = Some(0);
    session.needs_pending = true;
    session
        .needs_down(1, "controlled cleanup", &mut super::super::Alone)
        .unwrap();
    session
        .retry_needs(
            &super::super::Screen::default(),
            &super::super::SuperviseOpts::default(),
            &mut super::super::Alone,
        )
        .unwrap();
    drop(session);
    assert!(
        !adapter.typed.is_empty(),
        "direct cleanup reached the typed metadata precheck"
    );
    assert!(
        adapter
            .typed
            .iter()
            .all(|(scope, revision, _)| *scope == Scope::TurnEnd && *revision == 41)
    );
    assert!(
        adapter
            .legacy
            .iter()
            .all(|words| words.last().map(String::as_str) == Some("meta")),
        "only ownership observations use the ordinary read lane"
    );
}

#[test]
fn status_authority_rejects_unknown_duplicate_and_noncanonical_fields() {
    let good = "OK supervisor=grok-alab@42 supervisor_contract=3 supervisor_scope=upgrade supervisor_revision=41";
    assert_eq!(
        BuiltinAuthority::from_status(good, "host@7"),
        BuiltinAuthority::complement(41, [Scope::Upgrade])
    );
    for bad in [
        good.replace("supervisor_revision=41", ""),
        format!("{good} supervisor_revision=42"),
        good.replace("revision=41", "revision=041"),
        good.replace("scope=upgrade", "scope=upgrade,upgrade"),
        good.replace("contract=3", "contract=4"),
        format!("{good}\nOK another=reply"),
    ] {
        assert_eq!(
            BuiltinAuthority::from_status(&bad, "host@7"),
            BuiltinAuthority::Unknown,
            "{bad}"
        );
    }
    assert_eq!(
        BuiltinAuthority::from_status(
            "OK supervisor=- supervisor_contract=- supervisor_scope=- supervisor_revision=42",
            "host@7"
        ),
        BuiltinAuthority::complement(42, [])
    );
}

#[test]
fn complement_and_recorded_authority_preserve_all_duties_and_revision() {
    for authority in [
        BuiltinAuthority::Unsupported,
        BuiltinAuthority::Unknown,
        BuiltinAuthority::Legacy,
        BuiltinAuthority::Exclusive,
        BuiltinAuthority::complement(41, [Scope::Upgrade]),
        BuiltinAuthority::complement(42, []),
    ] {
        assert_eq!(
            BuiltinAuthority::from_tape(&authority.to_tape()).unwrap(),
            authority
        );
    }
    let authority = BuiltinAuthority::complement(41, [Scope::Upgrade]);
    assert!(!authority.keeps(Scope::Upgrade));
    for duty in [
        Scope::Approvals,
        Scope::Questions,
        Scope::TurnEnd,
        Scope::Walls,
    ] {
        assert!(authority.keeps(duty));
    }
    assert!(BuiltinAuthority::from_tape(&[4, 0]).is_err());
    assert!(BuiltinAuthority::from_tape(&[4, 0, 0, 0, 0, 0, 0, 0, 0, 64]).is_err());
    assert!(BuiltinAuthority::from_tape(&[2, 0]).is_err());
}

#[test]
fn semantic_duty_restores_after_nested_error_and_unwind() {
    let duty = DutyContext::default();
    let outer = duty.enter(Scope::Approvals, Some(41));
    let result = std::panic::catch_unwind(|| {
        let _inner = duty.enter(Scope::Questions, Some(42));
        assert_eq!(duty.get(), Some(Scope::Questions));
        panic!("controlled semantic-operation failure");
    });
    assert!(result.is_err());
    assert_eq!(duty.get(), Some(Scope::Approvals));
    assert_eq!(duty.revision(), Some(41));
    drop(outer);
    assert_eq!(duty.get(), None);
}
