// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Conformance tests binding `HarnessUpgradeQuiet` to the quiet round's reducer
//! (`upgrade::quiet_step` through `requested_step` and `break_step`, `tell_due`, `quiet_move`,
//! `own_yields`, `deadline_voids`).
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
/// rung: the agent's real work at an idle point is a process under it, or a status its in-process
/// work keeps busy past the turn end ([`upgrade::status_lags`]); and a turn that runs is its busy
/// footer, or, with a shell of the agent's own alive, the `shell` status that has not yet lagged
/// its turn's end (no footer, no idle look in a row).
fn looks(s: &State) -> Vec<Facts> {
    let f = facts(s);
    if s["posture"] == 2 && s["work"] == 2 {
        let lagging = Facts {
            status: "busy".to_string(),
            background: Vec::new(),
            ..f.clone()
        };
        assert!(upgrade::status_lags(&lagging), "{lagging:?}");
        vec![f, lagging]
    } else if s["posture"] == 0 && s["work"] > 0 {
        let fresh = Facts {
            status: "shell".to_string(),
            busy_footer: false,
            ..f.clone()
        };
        assert!(!upgrade::status_lags(&fresh), "{fresh:?}");
        vec![f, fresh]
    } else {
        vec![f]
    }
}

/// A quiet round's look at state `s`, in a session its autopilot carries, at the land rung. With
/// a shell of the agent's own alive its status is `shell`, never `idle`: at an idle point it is
/// the status that lags the turn's end, which is what the look reads there.
fn facts(s: &State) -> Facts {
    let mut f = Facts {
        status: if s["work"] > 0 { "shell" } else { "idle" }.to_string(),
        status_age_s: 3_600,
        composer_empty: true,
        background: if s["work"] > 0 {
            vec!["zsh".to_string()]
        } else {
            Vec::new()
        },
        waits_only: s["work"] == 1,
        deadline_set: s["tells"] > 0,
        work_due: s["due"] == 1,
        quiet_s: 3_600,
        idle_looks: upgrade::IDLE_LOOKS,
        rung: upgrade::Rung::Land,
        still_s: 3_600,
        quiet: true,
        may_tell: true,
        attended: s["near"] == 1,
        own_hold_s: if s["held"] == 1 {
            upgrade::WORK_TELL_S
        } else {
            upgrade::WORK_TELL_S - 1
        },
        tells: u32::try_from(s["tells"]).expect("tells"),
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
        upgrade::break_step(step, f)
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
    let (mut told, mut moved, mut over_waits, mut at_deadline, mut voided) = (0, 0, 0, 0, 0);
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
            today.action_enabled("DeadlineVoids", s),
            upgrade::deadline_voids(&f),
            "a deadline is voided exactly where its hold is broken: {s:?}\n{f:?}"
        );
        // NEGATIVE CONTROL: the person and the wall hidden from the void, and a deadline
        // outlives the hold it was told over.
        let hidden = Facts {
            limited: false,
            login: false,
            ..unseen(&f)
        };
        if upgrade::deadline_voids(&hidden) != today.action_enabled("DeadlineVoids", s) {
            caught.insert("a deadline outlives a person or a wall");
        }
        voided += usize::from(upgrade::deadline_voids(&f));
        let over = today.action_enabled("MoveOverOwn", s);
        assert_eq!(
            today.action_enabled("MoveAtIdle", s) || over,
            step == Step::Terminate,
            "{s:?}: {step:?}\n{f:?}"
        );
        assert_eq!(
            over,
            step == Step::Terminate && upgrade::own_yields(&f).is_some(),
            "the move over the agent's own is own_yields': {s:?}: {step:?}\n{f:?}"
        );
        if over {
            let mut after = s.clone();
            assert!(today.fire("MoveOverOwn", &mut after));
            assert_eq!(
                after["carried"] == 1,
                upgrade::own_yields(&f) == Some(upgrade::Yield::Waits),
                "waits are carried exactly where the move is over waits: {s:?}"
            );
        }
        assert!(
            !matches!(step, Step::Announce | Step::GiveUp | Step::Fresh),
            "{s:?}: a quiet pending round asks nothing: {step:?}"
        );
        if tells(&step) {
            let mut after = s.clone();
            assert!(today.fire("TellAtPoint", &mut after));
            let typed = Facts {
                tells: f.tells + 1,
                deadline_set: true,
                ..f.clone()
            };
            assert_eq!(quiet_projection(&typed), {
                let mut want = projected.clone();
                want.insert("tells", after["tells"]);
                want
            });
            // The deadline the line names passing is the clock's step over that stamp.
            assert!(today.fire("DeadlinePasses", &mut after));
            assert_eq!(
                quiet_projection(&Facts {
                    work_due: true,
                    ..typed
                })["due"],
                after["due"]
            );
            told += 1;
        }
        if step == Step::Terminate {
            moved += 1;
            if upgrade::own_yields(&f) == Some(upgrade::Yield::Waits) {
                over_waits += 1;
            } else if upgrade::own_yields(&f).is_some() && s["work"] == 2 {
                at_deadline += 1;
            }
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
        let controls: [(&str, &str, Step); 8] = [
            (
                "tells below the bound",
                "TellAtPoint",
                real(&Facts {
                    own_hold_s: upgrade::WORK_TELL_S,
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
                    // The most tells still under the cap: the real reducer may tell here.
                    tells: upgrade::MAX_TELLS - 1,
                    deadline_set: false,
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
                    status: "idle".to_string(),
                    ..f.clone()
                }),
            ),
            // The hold before `own_yields`: the agent's own work holds the round with no bound.
            (
                "own work holds",
                "MoveAtIdle",
                real(&Facts {
                    waits_only: false,
                    work_due: false,
                    ..f.clone()
                }),
            ),
            // A deadline in force that no line stamped.
            (
                "a deadline untold",
                "MoveAtIdle",
                real(&Facts {
                    work_due: true,
                    ..f.clone()
                }),
            ),
        ];
        for (name, action, control) in controls {
            let (does, want) = if action == "TellAtPoint" {
                (tells(&control), today.action_enabled(action, s))
            } else {
                (
                    control == Step::Terminate,
                    today.action_enabled("MoveAtIdle", s) || over,
                )
            };
            if does != want {
                caught.insert(name);
            }
        }
    }
    assert!(
        told > 0 && moved > 0 && over_waits > 0 && at_deadline > 0 && voided > 0,
        "{told} {moved} {over_waits} {at_deadline} {voided}"
    );
    assert_eq!(
        caught,
        BTreeSet::from([
            "a deadline outlives a person or a wall",
            "a deadline untold",
            "moves at a break",
            "moves over work",
            "own work holds",
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
        [
            ("DeadlineVoids", "deadline_voids"),
            ("MoveAtIdle", "quiet_move"),
            ("MoveOverOwn", "own_yields"),
            ("TellAtPoint", "tell_due")
        ]
    );
    let entered = xref::entered_anchor_ids();
    for id in [
        "HarnessUpgradeQuiet::DeadlineVoids @ deadline_voids",
        "HarnessUpgradeQuiet::MoveAtIdle @ quiet_move",
        "HarnessUpgradeQuiet::MoveOverOwn @ own_yields",
        "HarnessUpgradeQuiet::TellAtPoint @ tell_due",
    ] {
        assert!(entered.contains(id), "{id} entered: {entered:?}");
    }
}
