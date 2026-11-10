// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// THE TABLE OF EVERY WRITE PATH (H6 rule 5, the E2E-14 trace's list): each row runs a write
// path's own scenario three times, as an unconfirmed agent's (the host's act for a copy no seam
// names), as a confirmed agent's (the host's act for the lab stand-in the dev seam registers,
// each read through `agent_image::proof_among`) and as today's name-only admission's (the host
// confirms nothing: the negative control). An unconfirmed agent gets no
// input verb at all, and a box is escalated instead; the confirmed agent and the name-only
// admission each get the row's own writes.

/// One write path: its scenario runs on the thread's [`Mode`] and names the suffixes of the
/// writes it makes when it acts.
struct Row {
    name: &'static str,
    run: fn() -> &'static [&'static str],
    /// The scenario draws a box the loop decides (`decide_box`): observed, it is escalated.
    boxed: bool,
}

const ROWS: &[Row] = &[
    Row {
        name: "approval yes",
        run: row_approval,
        boxed: true,
    },
    Row {
        name: "decline",
        run: super::engine::box_hold::observe_row_decline,
        boxed: true,
    },
    Row {
        name: "focus/trust dialog",
        run: super::engine::observe_row_trust,
        boxed: true,
    },
    Row {
        name: "launch folder-trust, answered at once",
        run: super::engine::observe_row_launch,
        boxed: true,
    },
    Row {
        name: "question answer",
        run: super::engine::observe_row_question,
        boxed: true,
    },
    Row {
        name: "usage-limit dialog",
        run: super::engine::observe_row_limit_dialog,
        boxed: true,
    },
    Row {
        name: "autocompact dialog",
        run: super::engine::observe_row_autocompact,
        boxed: true,
    },
    Row {
        name: "auto-mode dialog",
        run: super::engine::observe_row_auto_mode,
        boxed: true,
    },
    Row {
        name: "Claude in Chrome intro",
        run: super::engine::observe_row_chrome_intro,
        boxed: true,
    },
    Row {
        // The login dialog is the picker of aterm's own `/login`: observed, no `/login` goes, so
        // no picker of aterm's is drawn to decide.
        name: "login dialog",
        run: super::turn_end::observe_row_login_dialog,
        boxed: false,
    },
    Row {
        name: "survey 0",
        run: row_survey,
        boxed: false,
    },
    Row {
        name: "survey's backspace",
        run: row_backspace,
        boxed: false,
    },
    Row {
        name: "turn-end turn submit",
        run: super::turn_end::observe_row_continue,
        boxed: false,
    },
    Row {
        name: "/usage probe, then the turn end's send and Enter",
        run: super::turn_end::observe_row_usage_probe,
        boxed: false,
    },
    Row {
        name: "Cruise background carry",
        run: super::turn_end::hold_journal::occupancy::write::cruise_carry_loop::observe_row_cruise_carry,
        boxed: false,
    },
    Row {
        name: "woken-turn Esc",
        run: super::turn_end::woken_turn_loop::observe_row_woken_esc,
        boxed: false,
    },
    Row {
        name: "wall-driven /compact",
        run: super::wall_confirm_loop::observe_row_compact,
        boxed: false,
    },
    Row {
        name: "wall-driven /login",
        run: super::turn_end::observe_row_login,
        boxed: false,
    },
];

/// What a row's mocks heard in one mode.
#[derive(Debug)]
struct Seen {
    /// Every input verb that reached a mock.
    inputs: Vec<String>,
    /// Whether a mock heard an escalation: an attention raised or the session log's escalate.
    escalated: bool,
    /// How often the loop asked the host for the session's act.
    asked: usize,
    /// How many input verbs the gate in [`Session::call`] refused (`observe::REFUSED`).
    refused: usize,
}

/// Restores the thread's table on every way out of a row, a panic's included.
struct Unset;

impl Drop for Unset {
    fn drop(&mut self) {
        TABLE.with_borrow_mut(|t| *t = None);
    }
}

fn seen(mode: Mode, run: fn() -> &'static [&'static str]) -> (Seen, &'static [&'static str]) {
    TABLE.with_borrow_mut(|t| *t = Some((mode, Vec::new())));
    let unset = Unset;
    super::observe::REFUSED.set(0);
    let wrote = run();
    let refused = super::observe::REFUSED.get();
    let heard = TABLE.with_borrow_mut(|t| t.take().map(|(_, heard)| heard).unwrap_or_default());
    drop(unset);
    let words =
        |line: &String| -> Vec<String> { line.split_whitespace().map(str::to_string).collect() };
    let inputs = heard
        .iter()
        .filter(|line| {
            let words = words(line);
            let words: Vec<&str> = words.iter().map(String::as_str).collect();
            claim::is_input(&words)
        })
        .cloned()
        .collect();
    let escalated = heard.iter().any(|line| {
        line.contains("meta set attention") || line.contains("mark - act word=escalate")
    });
    let asked = heard.iter().filter(|line| *line == TABLE_ASKED).count();
    (
        Seen {
            inputs,
            escalated,
            asked,
            refused,
        },
        wrote,
    )
}

/// A guarded press on a read-only Bash box ([`supervise_auto_approves_reads_guarded_and_hands_over_writes`]).
fn row_approval() -> &'static [&'static str] {
    let mut m = fenced(vec![bash_one_row(), busy_screen(), idle_screen()]);
    let mut s = session(&mut m, Some("@s-9".to_string()));
    let _ = s.supervise(&auto(30));
    &[" 1"]
}

/// The survey's guarded `0` ([`dismiss_surveys_presses_one_guarded_0`]).
fn row_survey() -> &'static [&'static str] {
    let mut m = Mock::new(vec![with_survey(idle_screen()), idle_screen()]);
    m.vanish_after = Some(1);
    let _ = watch_lines(&mut m, &surveys(auto(30), true));
    &[" 0"]
}

/// The backspace of a survey's `0` that landed in the composer
/// ([`a_0_that_landed_in_the_composer_is_backspaced_not_dismissed`]).
fn row_backspace() -> &'static [&'static str] {
    let mut zero = idle_screen();
    let c = zero.iter().position(|r| r == "❯").expect("composer");
    zero[c] = "❯ 0".to_string();
    let mut m = Mock::new(vec![with_survey(idle_screen()), zero, idle_screen()]);
    m.vanish_after = Some(0);
    let _ = watch_lines(&mut m, &dismissing());
    &[" 0", " backspace"]
}

/// NEGATIVE CONTROLS: the name-only column is today's admission, which writes in every row; and
/// by hand (run): with the gate's line in [`Session::call`] removed, the observe column's
/// non-box rows write, and with `decide_box`'s observe escalation removed, its box rows write
/// nothing and escalate nothing for want of the press the gate refuses.
#[test]
fn every_write_path_types_nothing_into_an_unconfirmed_agent() {
    let mut wrong = Vec::new();
    for row in ROWS {
        let (observed, _) = seen(Mode::Observe, row.run);
        if !observed.inputs.is_empty() {
            wrong.push(format!(
                "{}: observed, wrote {:#?}",
                row.name, observed.inputs
            ));
        }
        if observed.asked == 0 {
            wrong.push(format!("{}: observed, never asked the host", row.name));
        }
        if row.boxed && !observed.escalated {
            wrong.push(format!("{}: observed, the box was not escalated", row.name));
        }
        for mode in [Mode::On, Mode::NameOnly] {
            let (acted, wrote) = seen(mode, row.run);
            for suffix in wrote {
                if !acted.inputs.iter().any(|line| line.ends_with(suffix)) {
                    wrong.push(format!(
                        "{} ({mode:?}): no write ending {suffix:?}: {:#?}",
                        row.name, acted.inputs
                    ));
                }
            }
            let asks = mode == Mode::On;
            if (acted.asked > 0) != asks {
                wrong.push(format!(
                    "{} ({mode:?}): asked the host {} times",
                    row.name, acted.asked
                ));
            }
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// An observed agent's turn end is decided as ever: the stop phrase that escalates for a
/// confirmed agent escalates for it too, and nothing is typed (the act is asked only of a
/// decision that would type). CONTROL: the name-only column escalates alike. NEGATIVE CONTROL
/// (by hand, run): an observed turn end decided as nothing (the watch-behind early return)
/// escalates nothing.
#[test]
fn an_observed_turn_end_still_escalates() {
    let (observed, _) = seen(Mode::Observe, super::turn_end::observe_stop_phrase);
    assert!(observed.inputs.is_empty(), "{observed:#?}");
    assert!(observed.escalated, "{observed:#?}");
    let (named, _) = seen(Mode::NameOnly, super::turn_end::observe_stop_phrase);
    assert!(named.escalated, "{named:#?}");
}

/// An observed turn end withholds its continuation at the decision, so the loop sends nothing
/// the gate must refuse: no `ERR busy supervisor_act=observe` round trip, and the decision is
/// Nothing (trace check `agent-observed`) though the loop asked the host's act. NEGATIVE CONTROL
/// (run): with the turn end's withholding removed, the gate refuses the continuation's `turn`.
#[test]
fn an_observed_turn_end_withholds_its_continue_before_the_gate() {
    let (observed, _) = seen(Mode::Observe, super::turn_end::observe_row_continue);
    assert!(observed.inputs.is_empty(), "{observed:#?}");
    assert!(observed.asked > 0, "{observed:#?}");
    assert_eq!(observed.refused, 0, "{observed:#?}");
    let (named, wrote) = seen(Mode::NameOnly, super::turn_end::observe_row_continue);
    assert!(
        wrote
            .iter()
            .all(|suffix| named.inputs.iter().any(|line| line.ends_with(suffix))),
        "CONTROL: the name-only admission continues: {named:#?}"
    );
}

/// An observed agent is never restarted: a restart ends the process and the host types its
/// relaunch line and continuation through its own connection, past the gate, so the turn end
/// withholds the restart it decides (here a usage wall with an account to move to). CONTROL: a
/// confirmed agent and the name-only admission each ask the host's restart. NEGATIVE CONTROL
/// (run): with a restart left out of what an observed turn end withholds, the observed agent is
/// restarted.
#[test]
fn an_observed_agent_is_never_restarted() {
    for (mode, restarts) in [(Mode::Observe, 0), (Mode::On, 1), (Mode::NameOnly, 1)] {
        TABLE.with_borrow_mut(|t| *t = Some((mode, Vec::new())));
        let unset = Unset;
        let asked = super::turn_end::observe_row_restart();
        drop(unset);
        assert_eq!(asked.len(), restarts, "{mode:?}: {asked:?}");
    }
}

#[test]
fn a_scoped_account_event_reaches_the_harness_while_observed() {
    TABLE.with_borrow_mut(|t| *t = Some((Mode::Observe, Vec::new())));
    let unset = Unset;
    let asked = super::turn_end::observe_row_scoped_restart();
    drop(unset);
    assert_eq!(asked.len(), 1, "{asked:?}");
}
