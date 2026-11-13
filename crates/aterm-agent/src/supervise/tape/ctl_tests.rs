// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn actual_acts_cursor_keeps_typed_input_as_the_exact_closing_act() {
    use crate::supervise::tape::{
        acts::{ActClass, ActsCursor, Refusal, act_class},
        doors,
    };
    use astream_effects::EffectRecord;
    let request = duty_request(Scope::Approvals, 41, &["@s-duty", "key", "1"]);
    let record = EffectRecord::Exchange {
        door: doors::CTL_DUTY.to_string(),
        request: request.clone(),
        reply: DutyReply(Some(Ok(CtlReply {
            code: 0,
            stdout: "OK\n".to_string(),
            stderr: String::new(),
        })))
        .to_tape(),
    };
    assert_eq!(act_class(doors::CTL_DUTY, &request), ActClass::Act);
    assert_eq!(act_class(doors::CTL_DUTY, b"bad envelope"), ActClass::Act);
    let mut cursor = ActsCursor::new(vec![record], vec![None]);
    assert_eq!(cursor.acts_left(), (Some(0), 1));
    for changed in [
        duty_request(Scope::Questions, 41, &["@s-duty", "key", "1"]),
        duty_request(Scope::Approvals, 42, &["@s-duty", "key", "1"]),
    ] {
        assert!(matches!(
            cursor.exchange(doors::CTL_DUTY, &changed),
            Err(Refusal::ActDiffers { .. })
        ));
        assert_eq!(
            cursor.acts_left(),
            (Some(0), 1),
            "a changed scope/revision cannot discard its act"
        );
    }
    assert!(
        matches!(
            cursor.exchange(doors::CTL, &codec::ctl_request(&["@s-duty", "key", "1"])),
            Err(Refusal::ActDiffers { .. })
        ),
        "ordinary fallback is a different act"
    );
    cursor.exchange(doors::CTL_DUTY, &request).unwrap();
    assert_eq!(cursor.acts_left(), (None, 0));
}

#[test]
fn strict_and_acts_profiles_report_changed_typed_revision_as_a_decision() {
    use crate::supervise::tape::{doors, header::VERBATIM, replay::ReplayCore, report::Class};
    use astream_effects::EffectRecord;
    for acts in [false, true] {
        let record = EffectRecord::Exchange {
            door: doors::CTL_DUTY.to_string(),
            request: duty_request(Scope::Approvals, 41, &["@s-duty", "key", "1"]),
            reply: DutyReply(Some(Ok(CtlReply {
                code: 0,
                stdout: "OK\n".to_string(),
                stderr: String::new(),
            })))
            .to_tape(),
        };
        let core = if acts {
            ReplayCore::start_acts(vec![record], vec![None], VERBATIM, VERBATIM, "test", None)
        } else {
            ReplayCore::start(vec![record], VERBATIM, VERBATIM, "test", None)
        };
        let mut replay = ReplayCtl::new(Fx::replay(std::sync::Arc::clone(&core)), doors::CTL);
        assert!(
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                replay.call_for_duty(Scope::Approvals, 42, &["@s-duty", "key", "1"])
            }))
            .is_err()
        );
        assert_eq!(
            core.diverged().unwrap().class,
            Class::Decision,
            "acts={acts}"
        );
    }
}

#[test]
fn typed_envelopes_scrub_through_their_codec_and_text_roundtrips_all_reply_variants() {
    use crate::supervise::tape::{codec::Item, corpus::Scrub, doors, text};
    use astream_effects::EffectRecord;
    let authority = BuiltinAuthority::complement(41, [Scope::Upgrade]).to_tape();
    let items = vec![
        Item::effect(EffectRecord::Exchange {
            door: doors::CTL_AUTHORITY.to_string(),
            request: codec::ctl_request(&["@s-duty"]),
            reply: authority.clone(),
        }),
        Item::effect(EffectRecord::Exchange {
            door: doors::CTL_DUTY.to_string(),
            request: duty_request(
                Scope::TurnEnd,
                41,
                &["@s-duty", "send", "cat /Users//private/file"],
            ),
            reply: DutyReply(Some(Err("refused /Users//private/file".to_string()))).to_tape(),
        }),
        Item::effect(EffectRecord::Exchange {
            door: doors::CTL_DUTY.to_string(),
            request: duty_request(Scope::TurnEnd, 41, &["@s-duty", "send", "x"]),
            reply: DutyReply(None).to_tape(),
        }),
    ];
    let mut scrub = Scrub::new();
    scrub
        .path(std::path::Path::new("/Users//private"), "/lab/home")
        .unwrap();
    let (_, mapped) = scrub.tape(None, &items).unwrap();
    let EffectRecord::Exchange { reply, .. } = mapped[0].record().unwrap() else {
        panic!("authority")
    };
    assert_eq!(
        *reply, authority,
        "authority revision/mask bytes are not text-scrubbed"
    );
    let EffectRecord::Exchange {
        door,
        request,
        reply,
    } = mapped[1].record().unwrap()
    else {
        panic!("typed request")
    };
    let (scope, revision, args) = duty_parts(request).unwrap();
    assert_eq!((scope, revision), (Scope::TurnEnd, 41));
    assert_eq!(args[2], "cat /lab/home/file");
    assert_eq!(
        control_reply(door, reply).unwrap(),
        Some(Err("refused /lab/home/file".to_string()))
    );
    let text = text::render(None, &mapped);
    assert!(!text.contains("/Users//private"));
    let (_, decoded) = text::parse(&text).unwrap();
    assert_eq!(
        decoded, mapped,
        "pretty typed replies and raw unsupported replies both preserve exact bytes"
    );
}

#[test]
fn recorded_duty_observation_and_exact_typed_act_replay_without_a_live_host() {
    use crate::supervise::fx::ClockSource;
    use crate::supervise::tape::{header::VERBATIM, replay::ReplayCore, testing::MemoryTape};
    struct Private;
    impl Ctl for Private {
        fn call(&mut self, _: &[&str]) -> Result<CtlReply, String> {
            panic!("no legacy fallback")
        }
        fn builtin_authority(&mut self, _: &str) -> BuiltinAuthority {
            BuiltinAuthority::complement(41, [Scope::Upgrade])
        }
        fn call_for_duty(
            &mut self,
            duty: Scope,
            revision: u64,
            _: &[&str],
        ) -> Option<Result<CtlReply, String>> {
            assert_eq!((duty, revision), (Scope::Approvals, 41));
            Some(Ok(CtlReply {
                code: 0,
                stdout: "OK\n".to_string(),
                stderr: String::new(),
            }))
        }
    }
    let clock = Fx::over(ClockSource::Scripted(std::sync::Arc::default()));
    let tape = MemoryTape::over(&clock);
    let mut private = Private;
    let mut live = TapedCtl::new(&mut private, tape.fx.clone(), super::super::doors::CTL);
    assert_eq!(live.builtin_authority("@s-duty").revision(), Some(41));
    assert!(
        live.call_for_duty(Scope::Approvals, 41, &["@s-duty", "key", "1"])
            .unwrap()
            .unwrap()
            .ok()
    );
    drop(live);
    let read = tape.finish("done");
    let records: Vec<_> = read
        .items
        .iter()
        .filter_map(|item| item.record().cloned())
        .collect();
    assert_eq!(
        records.len(),
        2,
        "authority and actual semantic request are separate evidence"
    );
    let replay = ReplayCore::start(records.clone(), VERBATIM, VERBATIM, "test", None);
    let mut offline = ReplayCtl::new(Fx::replay(replay), super::super::doors::CTL);
    assert_eq!(offline.builtin_authority("@s-duty").revision(), Some(41));
    assert!(
        offline
            .call_for_duty(Scope::Approvals, 41, &["@s-duty", "key", "1"])
            .unwrap()
            .unwrap()
            .ok()
    );
    let replay = ReplayCore::start(records, VERBATIM, VERBATIM, "test", None);
    let mut mutant = ReplayCtl::new(Fx::replay(replay), super::super::doors::CTL);
    mutant.builtin_authority("@s-duty");
    assert!(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            mutant.call_for_duty(Scope::Approvals, 42, &["@s-duty", "key", "1"])
        }))
        .is_err(),
        "a different revision cannot consume the recorded typed act"
    );
}

/// The host's act for a session is an observation a tape records and a replay answers
/// offline; a corpus scrub maps its session word and keeps its one byte. NEGATIVE CONTROL
/// (by hand, run): `TapedCtl::agent_act` calling the inner adapter unrecorded leaves no
/// record, and the replay panics at its unreachable live call.
#[test]
fn the_hosts_act_is_recorded_replayed_and_scrubbed() {
    use crate::supervise::fx::ClockSource;
    use crate::supervise::tape::{
        codec::Item, corpus::Scrub, doors, header::VERBATIM, replay::ReplayCore,
        testing::MemoryTape,
    };
    use astream_effects::EffectRecord;
    struct Host;
    impl Ctl for Host {
        fn call(&mut self, _: &[&str]) -> Result<CtlReply, String> {
            panic!("an act query sends no request")
        }
        fn agent_act(&mut self, sid: &str) -> Act {
            if sid == "@s-on" {
                Act::On
            } else {
                Act::Observe
            }
        }
    }
    let clock = Fx::over(ClockSource::Scripted(std::sync::Arc::default()));
    let tape = MemoryTape::over(&clock);
    let mut host = Host;
    let mut live = TapedCtl::new(&mut host, tape.fx.clone(), doors::CTL);
    assert_eq!(live.agent_act("@s-on"), Act::On);
    assert_eq!(live.agent_act("@s-jsmith"), Act::Observe);
    drop(live);
    let read = tape.finish("done");
    let records: Vec<_> = read
        .items
        .iter()
        .filter_map(|item| item.record().cloned())
        .collect();
    assert_eq!(records.len(), 2);
    assert!(
        records.iter().all(
            |r| matches!(r, EffectRecord::Exchange { door, .. } if door == doors::CTL_AGENT_ACT)
        )
    );
    let replay = ReplayCore::start(records.clone(), VERBATIM, VERBATIM, "test", None);
    let mut offline = ReplayCtl::new(Fx::replay(replay), doors::CTL);
    assert_eq!(offline.agent_act("@s-on"), Act::On);
    assert_eq!(offline.agent_act("@s-jsmith"), Act::Observe);

    let items: Vec<_> = records.into_iter().map(Item::effect).collect();
    let mut scrub = Scrub::new();
    scrub.word("jsmith", "lab").unwrap();
    let (_, mapped) = scrub.tape(None, &items).unwrap();
    let EffectRecord::Exchange { request, reply, .. } = mapped[1].record().unwrap() else {
        panic!("act")
    };
    assert_eq!(
        codec::ctl_args(request).unwrap(),
        ["@s-lab"],
        "the session word mapped"
    );
    assert_eq!(*reply, Act::Observe.to_tape());
    let bad = Item::effect(EffectRecord::Exchange {
        door: doors::CTL_AGENT_ACT.to_string(),
        request: codec::ctl_request(&["@s-on"]),
        reply: vec![7],
    });
    assert!(
        Scrub::new().tape(None, &[bad]).is_err(),
        "an act no build writes"
    );
}

/// The act classes the frame closes on: writes, attention, signals and
/// restarts are acts; a claim's renewal, a read and a wait are not, and a
/// selector in front changes nothing.
#[test]
fn acts_and_waits_are_told_apart_by_their_verb() {
    for act in [
        &["key", "1"][..],
        &["@s-1", "send", "hi"],
        &["turn", "go"],
        &["post", "@s-2", "x"],
        &["meta", "set", "attention", "x"],
        &["meta", "unset", "attention"],
        &["signal", "INT"],
        &["restart", "--pid", "9"],
    ] {
        assert!(is_act(act), "{act:?} is an act");
        assert!(!is_wait(act), "{act:?} is no wait");
    }
    for other in [
        &["text", "--json", "tail=40"][..],
        &["meta", "set", "supervisor", "aterm-supervise:1"],
        &["meta", "unset", "supervisor"],
        &["status"],
        &["@s-1", "who"],
        &["story", "approved"],
    ] {
        assert!(!is_act(other), "{other:?} is no act");
        assert!(!is_wait(other), "{other:?} is no wait");
    }
    assert!(is_wait(&["@s-1", "await", "seq", "4", "timeout", "20000"]));
    assert!(!is_act(&["await", "seq", "4"]));
}
