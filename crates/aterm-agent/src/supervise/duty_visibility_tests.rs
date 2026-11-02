// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The actual server advances supervisor_revision on EVERY accepted claim set, including
//! an unchanged TTL renewal. This transport seam models that effect rather than manually
//! refreshing the operation's revision or leaving the authority stub unchanged.

use super::*;
use crate::supervise::fx::{ClockSource, Fx, VirtualClock};
use std::time::{Duration, Instant};

struct RevisionAdapter {
    authority: Arc<Mutex<BuiltinAuthority>>,
    holder: Option<String>,
    sets: usize,
    unsets: usize,
    ordinary: Vec<Vec<String>>,
    typed: Vec<(Scope, u64, bool)>,
}

impl RevisionAdapter {
    fn new(authority: Arc<Mutex<BuiltinAuthority>>) -> Self {
        Self {
            authority,
            holder: None,
            sets: 0,
            unsets: 0,
            ordinary: Vec::new(),
            typed: Vec::new(),
        }
    }

    fn no_legacy_input(&self) {
        assert!(
            self.ordinary
                .iter()
                .all(|args| !super::super::claim::is_input(
                    &args.iter().map(String::as_str).collect::<Vec<_>>()
                ))
        );
    }
}

fn reply(ok: bool, stdout: String) -> CtlReply {
    CtlReply {
        code: i32::from(!ok),
        stdout,
        stderr: if ok { "" } else { "ERR denied" }.into(),
    }
}

impl Ctl for RevisionAdapter {
    fn call(&mut self, args: &[&str]) -> Result<CtlReply, String> {
        self.ordinary
            .push(args.iter().map(|word| word.to_string()).collect());
        let words = args
            .iter()
            .copied()
            .filter(|word| !word.starts_with('@'))
            .collect::<Vec<_>>();
        match words.as_slice() {
            ["meta", "set", "supervisor", holder, ..] => {
                let mut authority = self.authority.lock().unwrap();
                let BuiltinAuthority::Complement { revision, taken: 0 } = *authority else {
                    return Ok(reply(false, String::new()));
                };
                *authority = BuiltinAuthority::complement(revision.wrapping_add(1), []);
                self.holder = Some((*holder).to_string());
                self.sets += 1;
            }
            ["meta", "unset", "supervisor", expected] => {
                let mut authority = self.authority.lock().unwrap();
                if let BuiltinAuthority::Complement { revision, taken: 0 } = *authority
                    && expected.strip_prefix("holder=") == self.holder.as_deref()
                {
                    *authority = BuiltinAuthority::complement(revision.wrapping_add(1), []);
                    self.holder = None;
                    self.unsets += 1;
                }
            }
            ["meta"] => {
                return Ok(reply(
                    true,
                    format!(
                        "OK supervisor={} autopilot=cruise\n",
                        self.holder.as_deref().unwrap_or("-")
                    ),
                ));
            }
            ["lease", "acquire", ..] => {
                return Ok(reply(true, "OK lease acquired\n".into()));
            }
            _ => {}
        }
        Ok(reply(true, "OK\n".into()))
    }

    fn builtin_authority(&mut self, _: &str) -> BuiltinAuthority {
        *self.authority.lock().unwrap()
    }

    fn call_for_duty(
        &mut self,
        duty: Scope,
        revision: u64,
        _: &[&str],
    ) -> Option<Result<CtlReply, String>> {
        let current = *self.authority.lock().unwrap();
        let accepted = current.revision() == Some(revision) && current.keeps(duty);
        self.typed.push((duty, revision, accepted));
        Some(Ok(reply(accepted, "OK\n".into())))
    }
}

fn hosted(adapter: &mut RevisionAdapter) -> Session<'_, RevisionAdapter> {
    let mut session = Session::new(adapter, Some("@s-visibility".into()));
    session.set_builtin_duties(true);
    session.set_fx(Fx::over(ClockSource::Virtual(Arc::new(VirtualClock::new(
        Instant::now(),
        0,
    )))));
    session.set_claim_timing(Duration::from_secs(20), 90_000);
    session
}

#[test]
fn first_empty_complement_act_uses_revision_after_actual_visibility_claim() {
    let authority = Arc::new(Mutex::new(BuiltinAuthority::complement(10, [])));
    let mut adapter = RevisionAdapter::new(Arc::clone(&authority));
    let mut session = hosted(&mut adapter);
    session.with_duty(Scope::Approvals, |session| {
        assert!(matches!(session.claim, Claim::Held { .. }));
        assert_eq!(session.duty.revision(), Some(11));
        // Same order as the actual under_hand path before its first typed request.
        assert!(
            session
                .call(&["lease", "acquire", "kind=turn"])
                .unwrap()
                .ok()
        );
        assert!(session.call(&["key", "1"]).unwrap().ok());
    });
    drop(session);
    assert_eq!(adapter.sets, 1);
    assert_eq!(adapter.typed, [(Scope::Approvals, 11, true)]);
    adapter.no_legacy_input();
}

#[test]
fn due_visibility_renewal_precedes_capture_and_never_splits_a_semantic_act() {
    let authority = Arc::new(Mutex::new(BuiltinAuthority::complement(10, [])));
    let mut adapter = RevisionAdapter::new(authority);
    let mut session = hosted(&mut adapter);
    session.claim_at_naming();
    session.fx.sleep(Duration::from_secs(20));
    session.with_duty(Scope::Approvals, |session| {
        assert_eq!(session.duty.revision(), Some(12));
        assert!(session.call(&["key", "1"]).unwrap().ok());
        session.fx.sleep(Duration::from_secs(20));
        assert!(
            !session.renew_claim_if_due(),
            "no self-transition inside the pinned act"
        );
        session.with_duty(Scope::Approvals, |session| {
            assert!(session.call(&["key", "enter"]).unwrap().ok());
        });
        session.with_duty(Scope::Questions, |session| {
            assert!(session.call(&["key", "2"]).unwrap().ok());
        });
        assert_eq!(session.duty.revision(), Some(12));
        assert!(session.call(&["key", "enter"]).unwrap().ok());
    });
    // Normal polling still renews promptly after the semantic producer has finished.
    assert!(session.call(&["status"]).unwrap().ok());
    session.with_duty(Scope::Approvals, |session| {
        assert_eq!(session.duty.revision(), Some(13));
        assert!(session.call(&["key", "1"]).unwrap().ok());
    });
    drop(session);
    assert_eq!(adapter.sets, 3);
    assert_eq!(
        adapter.typed,
        [
            (Scope::Approvals, 12, true),
            (Scope::Approvals, 12, true),
            (Scope::Questions, 12, true),
            (Scope::Approvals, 12, true),
            (Scope::Approvals, 13, true),
        ]
    );
    adapter.no_legacy_input();
}

#[test]
fn foreign_transition_refuses_remaining_unsent_input_without_refreshing_its_revision() {
    let authority = Arc::new(Mutex::new(BuiltinAuthority::complement(
        41,
        [Scope::Upgrade],
    )));
    let mut adapter = RevisionAdapter::new(Arc::clone(&authority));
    let mut session = hosted(&mut adapter);
    session.with_duty(Scope::Approvals, |session| {
        assert!(session.call(&["key", "1"]).unwrap().ok());
        *authority.lock().unwrap() = BuiltinAuthority::complement(42, [Scope::Upgrade]);
        assert!(!session.call(&["key", "enter"]).unwrap().ok());
        session.with_duty(Scope::Approvals, |session| {
            assert_eq!(session.duty.revision(), Some(41));
            assert!(!session.call(&["key", "enter"]).unwrap().ok());
        });
    });
    session.with_duty(Scope::Approvals, |session| {
        assert!(session.call(&["key", "1"]).unwrap().ok());
    });
    drop(session);
    assert_eq!(adapter.sets, 0);
    assert_eq!(
        adapter.typed,
        [
            (Scope::Approvals, 41, true),
            (Scope::Approvals, 41, false),
            (Scope::Approvals, 41, false),
            (Scope::Approvals, 42, true),
        ]
    );
    adapter.no_legacy_input();
}

#[test]
fn foreign_departure_acquires_visibility_before_next_act_and_off_releases_only_ours() {
    let authority = Arc::new(Mutex::new(BuiltinAuthority::complement(
        41,
        [Scope::Approvals],
    )));
    let mut adapter = RevisionAdapter::new(Arc::clone(&authority));
    let mut session = hosted(&mut adapter);
    session.with_duty(Scope::Approvals, |session| {
        assert!(!session.call(&["key", "1"]).unwrap().ok());
    });
    session.with_duty(Scope::Questions, |session| {
        assert!(session.call(&["key", "2"]).unwrap().ok());
    });
    *authority.lock().unwrap() = BuiltinAuthority::complement(42, []);
    session.with_duty(Scope::Approvals, |session| {
        assert_eq!(session.duty.revision(), Some(43));
        assert!(session.call(&["key", "1"]).unwrap().ok());
    });
    session.give_back_claim();
    session.refresh_builtin_authority();
    assert!(!session.renew_claim_if_due());
    assert!(matches!(session.claim, Claim::GivenBack { .. }));
    drop(session);
    assert_eq!(adapter.sets, 1);
    assert_eq!(adapter.unsets, 1);
    assert_eq!(
        adapter.typed,
        [(Scope::Questions, 41, true), (Scope::Approvals, 43, true)]
    );
    adapter.no_legacy_input();
}

#[test]
fn direct_metadata_cleanup_and_retry_never_acquire_visibility() {
    let authority = Arc::new(Mutex::new(BuiltinAuthority::complement(41, [])));
    let mut adapter = RevisionAdapter::new(authority);
    let mut session = hosted(&mut adapter);
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
    assert_eq!(adapter.sets, 0);
    assert_eq!(adapter.unsets, 0);
    assert!(!adapter.typed.is_empty());
    assert!(
        adapter
            .typed
            .iter()
            .all(|&(scope, revision, accepted)| scope == Scope::TurnEnd
                && revision == 41
                && accepted)
    );
    adapter.no_legacy_input();
}
