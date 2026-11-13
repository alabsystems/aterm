// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// A host that answers every request `OK` and says `act` for every session.
struct Host {
    act: Act,
    sent: Vec<String>,
    asked: usize,
}

impl Ctl for Host {
    fn call(&mut self, args: &[&str]) -> Result<CtlReply, String> {
        self.sent.push(args.join(" "));
        Ok(CtlReply {
            code: 0,
            stdout: "OK\n".to_string(),
            stderr: String::new(),
        })
    }
    fn agent_act(&mut self, _: &str) -> Act {
        self.asked += 1;
        self.act
    }
}

const INPUT: [&[&str]; 5] = [
    &["key", "1"],
    &["send", "--", "go on"],
    &["turn", "go on"],
    &["paste", "x"],
    &["feed-bin", "00"],
];

/// The one gate: while the host observes, no input verb reaches the transport, and the
/// reads, waits and attention writes an escalation needs still do; a confirmed agent, or a
/// loop whose host confirms nothing, sends each. NEGATIVE CONTROL (by hand, run): with the
/// gate's line in [`Session::call`] removed, every input row reaches the host.
#[test]
fn an_observed_agent_gets_no_input_and_keeps_its_escalation() {
    for (confirms, act) in [(true, Act::Observe), (true, Act::On), (false, Act::Observe)] {
        let mut host = Host {
            act,
            sent: Vec::new(),
            asked: 0,
        };
        let mut s = Session::new(&mut host, Some("@s-1".to_string()));
        s.set_confirms_agent(confirms);
        let typed = confirms && act == Act::Observe;
        for args in INPUT {
            let r = s.call(args).unwrap();
            assert_eq!(r.is_err("busy"), typed, "{confirms} {act:?} {args:?}");
        }
        for args in [
            &["text", "--json"][..],
            &["meta", "set", "attention", "needs-you"],
            &["status"],
        ] {
            assert!(s.call(args).unwrap().ok(), "{args:?}");
        }
        drop(s);
        let reached = host
            .sent
            .iter()
            .filter(|line| {
                INPUT
                    .iter()
                    .any(|a| *line == &format!("@s-1 {}", a.join(" ")))
            })
            .count();
        assert_eq!(
            reached,
            if typed { 0 } else { INPUT.len() },
            "{confirms} {act:?}"
        );
        assert_eq!(
            host.sent.len() - reached,
            3,
            "the reads and the attention write"
        );
        assert_eq!(
            host.asked,
            if confirms { INPUT.len() } else { 0 },
            "asked per input"
        );
    }
}

#[test]
fn a_loop_that_names_no_session_observes() {
    let mut host = Host {
        act: Act::On,
        sent: Vec::new(),
        asked: 0,
    };
    let mut s = Session::new(&mut host, None);
    s.set_confirms_agent(true);
    assert!(s.call(&["key", "1"]).unwrap().is_err("busy"));
    drop(s);
    assert!(host.sent.is_empty());
    assert_eq!(host.asked, 0);
}

#[test]
fn the_act_crosses_a_tape_as_one_byte() {
    for act in Act::ALL {
        assert_eq!(Act::from_tape(&act.to_tape()).unwrap(), act);
    }
    assert!(Act::from_tape(&[2]).is_err());
    assert!(Act::from_tape(&[]).is_err());
}

#[test]
fn the_refusal_backs_off_and_names_no_holder() {
    let r = CtlReply {
        code: 1,
        stdout: String::new(),
        stderr: OBSERVE_REFUSAL.to_string(),
    };
    assert!(r.is_err("busy"));
    assert!(!r.is_err("busy sink"));
    assert!(!r.is_err("busy input-unread"));
    assert!(
        !r.err_text()
            .split_whitespace()
            .any(|w| w.starts_with("supervisor=")),
    );
}
