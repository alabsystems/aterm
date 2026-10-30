// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Conformance tests binding `HarnessUpgradeQuiet` to the quiet round's reducer
//! (`upgrade::quiet_step` through `requested_step` and `break_step`, `tell_due`, `quiet_move`).
use std::collections::{BTreeSet, VecDeque};

use aterm_models_harness::harness_upgrade_quiet_model;
use aterm_spec::derive::Model;
use aterm_spec::interp::{self, State};

use super::*;
use crate::harness::upgrade::{Facts, Step};
use crate::harness::upgrade_drive::quiet_projection;

const TARGET: &str = "2.1.310";

fn main_today() -> Model {
    interp::with_buggy(&harness_upgrade_quiet_model(), 0)
}

fn reach(m: &Model) -> Vec<State> {
    let mut seen: BTreeSet<State> = BTreeSet::new();
    let mut queue = VecDeque::from([m.init_state()]);
    let mut out = Vec::new();
    while let Some(s) = queue.pop_front() {
        if !seen.insert(s.clone()) {
            continue;
        }
        for a in &m.actions {
            queue.extend(m.successors(a.name, &s));
        }
        out.push(s);
    }
    out
}

/// The looks a quiet round can make at state `s`, in a session its autopilot carries, at the land
/// rung: the agent's work at an idle point is a process under it, or a status its in-process work
/// keeps busy past the turn end ([`upgrade::status_lags`]).
fn looks(s: &State) -> Vec<Facts> {
    let f = facts(s);
    if s["posture"] == 2 && s["work"] == 1 {
        let lagging = Facts {
            status: "busy".to_string(),
            background: Vec::new(),
            ..f.clone()
        };
        assert!(upgrade::status_lags(&lagging), "{lagging:?}");
        vec![f, lagging]
    } else {
        vec![f]
    }
}

/// A quiet round's look at state `s`, in a session its autopilot carries, at the land rung.
fn facts(s: &State) -> Facts {
    let mut f = Facts {
        status: "idle".to_string(),
        status_age_s: 3_600,
        composer_empty: true,
        background: if s["work"] == 1 {
            vec!["zsh".to_string()]
        } else {
            Vec::new()
        },
        quiet_s: 3_600,
        idle_looks: upgrade::IDLE_LOOKS,
        rung: upgrade::Rung::Land,
        still_s: 3_600,
        quiet: true,
        may_tell: true,
        attended: s["near"] == 1,
        own_hold_s: if s["held"] == 1 {
            upgrade::OWN_TELL_S
        } else {
            upgrade::OWN_TELL_S - 1
        },
        tells: u32::try_from(s["tells"]).expect("tells"),
        told_s: if s["rest"] == 1 {
            upgrade::RETELL_S - 1
        } else {
            upgrade::RETELL_S
        },
        ..Facts::default()
    };
    match s["posture"] {
        0 => {
            f.status = "busy".to_string();
            f.status_age_s = 5;
            f.busy_footer = true;
            f.idle_looks = 0;
            f.quiet_s = 0;
        }
        1 => {
            f.status = "busy".to_string();
            f.idle_looks = 0;
            f.background_point = true;
        }
        3 => f.limited = true,
        4 => f.approval_box = true,
        _ => {}
    }
    f
}

/// The real step at `f`'s look: the reducer, and at a break the break's step.
fn real(f: &Facts) -> Step {
    let step = upgrade::requested_step(&Request::None, &Phase::Pending, f, false, NOW, TARGET);
    if f.background_point {
        upgrade::break_step(step)
    } else {
        step
    }
}

/// `f` with the person at the session hidden from the reducer.
fn unseen(f: &Facts) -> Facts {
    Facts {
        attended: false,
        approval_box: false,
        ..f.clone()
    }
}

fn tells(step: &Step) -> bool {
    matches!(step, Step::Tell(_))
}

#[test]
fn the_quiet_flow_is_the_derived_machine() {
    use aterm_spec::xref;
    assert!(xref::reset_entered_anchors(), "the evidence window opens");
    let today = main_today();
    let buggy = interp::with_buggy(&harness_upgrade_quiet_model(), 1);
    let states = reach(&today);
    assert!(
        states.len() > 100,
        "the machine has a space: {}",
        states.len()
    );
    let (mut told, mut moved) = (0, 0);
    let mut caught: BTreeSet<&str> = BTreeSet::new();
    for (s, f) in states
        .iter()
        .filter(|s| s["moved"] == 0)
        .flat_map(|s| looks(s).into_iter().map(move |f| (s, f)))
    {
        let projected = quiet_projection(&f);
        for (var, value) in &projected {
            assert_eq!(s[*var], *value, "{var} at {s:?}: {f:?}");
        }
        let step = real(&f);
        assert_eq!(
            today.action_enabled("TellAtPoint", s),
            tells(&step),
            "{s:?}: {step:?}\n{f:?}"
        );
        assert_eq!(
            today.action_enabled("MoveAtIdle", s),
            step == Step::Terminate,
            "{s:?}: {step:?}\n{f:?}"
        );
        assert!(
            !matches!(step, Step::Announce | Step::GiveUp | Step::Fresh),
            "{s:?}: a quiet pending round asks nothing: {step:?}"
        );
        if tells(&step) {
            let mut after = s.clone();
            assert!(today.fire("TellAtPoint", &mut after));
            let typed = Facts {
                tells: f.tells + 1,
                told_s: 0,
                ..f.clone()
            };
            assert_eq!(quiet_projection(&typed), {
                let mut want = projected.clone();
                want.insert("tells", after["tells"]);
                want.insert("rest", after["rest"]);
                want
            });
            told += 1;
        }
        if step == Step::Terminate {
            moved += 1;
        }
        for (mutant, moves) in [
            ("MoveOverWork", true),
            ("MoveAtABreak", true),
            ("TellOffAPoint", false),
            ("TellOverAPerson", false),
            ("TellsEarly", false),
            ("TellsPastTheCap", false),
        ] {
            if buggy.action_enabled(mutant, s) {
                let does = if moves {
                    step == Step::Terminate
                } else {
                    tells(&step)
                };
                assert!(!does, "{mutant} at {s:?}: the real step {step:?}");
            }
        }
        let controls: [(&str, &str, Step); 6] = [
            (
                "tells below the bound",
                "TellAtPoint",
                real(&Facts {
                    own_hold_s: upgrade::OWN_TELL_S,
                    ..f.clone()
                }),
            ),
            (
                "tells off a point",
                "TellAtPoint",
                real(&Facts {
                    background_point: s["posture"] <= 1,
                    busy_footer: false,
                    limited: false,
                    login: false,
                    ..f.clone()
                }),
            ),
            ("tells over a person", "TellAtPoint", real(&unseen(&f))),
            (
                "tells past the cap",
                "TellAtPoint",
                real(&Facts {
                    tells: f.tells.min(upgrade::MAX_TELLS - 1),
                    ..f.clone()
                }),
            ),
            (
                "moves at a break",
                "MoveAtIdle",
                if s["posture"] == 1 {
                    real(&Facts {
                        status: "idle".to_string(),
                        background_point: false,
                        ..f.clone()
                    })
                } else {
                    step
                },
            ),
            (
                "moves over work",
                "MoveAtIdle",
                real(&Facts {
                    background: Vec::new(),
                    ..f.clone()
                }),
            ),
        ];
        for (name, action, control) in controls {
            let does = if action == "TellAtPoint" {
                tells(&control)
            } else {
                control == Step::Terminate
            };
            if does != today.action_enabled(action, s) {
                caught.insert(name);
            }
        }
    }
    assert!(told > 0 && moved > 0, "{told} {moved}");
    assert_eq!(
        caught,
        BTreeSet::from([
            "moves at a break",
            "moves over work",
            "tells below the bound",
            "tells off a point",
            "tells over a person",
            "tells past the cap",
        ])
    );
    let mut anchored: Vec<(&str, &str)> = xref::refinements()
        .filter(|a| a.machine == "HarnessUpgradeQuiet")
        .map(|a| {
            assert_eq!(
                a.project,
                "aterm_agent::harness::upgrade_drive::quiet_projection"
            );
            (a.action, a.rust_method)
        })
        .collect();
    anchored.sort_unstable();
    assert_eq!(
        anchored,
        [("MoveAtIdle", "quiet_move"), ("TellAtPoint", "tell_due")]
    );
    let entered = xref::entered_anchor_ids();
    for id in [
        "HarnessUpgradeQuiet::MoveAtIdle @ quiet_move",
        "HarnessUpgradeQuiet::TellAtPoint @ tell_due",
    ] {
        assert!(entered.contains(id), "{id} entered: {entered:?}");
    }
}
