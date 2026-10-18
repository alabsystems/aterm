// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

/// A VOLUME SHORT WHILE CLEANUP IS OFF IS NOT THE PASSES' LINE (review
/// 30): no pass ran, so its line has its own key — dismissing one never
/// hides the other — and carries the settings' action, where the passes'
/// line has none.
#[test]
fn a_volume_short_while_cleanup_is_off_is_not_the_passes_line() {
    let passes = super::disk_low_entry(3, 17, &["~/a 9 GiB (a)".to_string()]);
    let off = super::disk_low_cleanup_off_entry(3, 17);
    let settings = super::disk_settings_entry(&["disk.apply is not read".to_string()]);
    assert_ne!(off.key(), passes.key());
    assert!(passes.mover().is_none());
    assert!(off.mover().is_some());
    assert_eq!(off.mover(), settings.mover());
}

/// What the digest reads of `entry`: its title, its reason, its action and
/// the detail lines behind them.
fn entry_words(entry: &Entry) -> (String, String, String, Vec<String>) {
    let msg = entry
        .message()
        .unwrap_or_else(|fault| panic!("{}: {fault:?}", entry.key()));
    let aterm_messages::need::Need::Later(ask) = msg.need() else {
        panic!("{}: an entry is a Later ask", entry.key());
    };
    let (why, then) = (ask.why_words(), ask.then().words().to_string());
    assert_eq!(msg.detail[..2], [why.clone(), then.clone()]);
    (msg.title.clone(), why, then, msg.detail[2..].to_vec())
}

fn signed_out(labels: &[&str]) -> Entry {
    let labels: Vec<String> = labels.iter().map(|l| (*l).to_string()).collect();
    accounts_signed_out_entry(&labels).expect("a label is an entry")
}

/// A LABEL CANNOT FAULT THE SIGNED-OUT ENTRY: a label is the owner's own
/// word, and the entry's lints read words — the reason for a stem anywhere
/// in it, the action for a snooze that leads it and for a second action.
/// Labels that are or hold those words build the same title and the same
/// reason as any other, and an action that ENDS in the label. NEGATIVE
/// CONTROL: each of those labels, written where its lint reads (into the
/// reason; leading the action; inside it), faults the entry.
#[test]
fn a_label_cannot_fault_the_signed_out_entry() {
    use aterm_messages::exceptions::EntryFault;
    #[derive(Clone, Copy, Debug)]
    enum Written {
        Reason,
        Leading,
        Inside,
    }
    let (title, why, _, _) = entry_words(&signed_out(&["main"]));
    for (label, where_it_faults) in [
        ("soon", Written::Reason),
        ("retry", Written::Reason),
        ("soon-1", Written::Reason),
        ("x.retrying", Written::Reason),
        ("self-heal", Written::Reason),
        ("0-automatically", Written::Reason),
        ("wait", Written::Leading),
        ("not", Written::Leading),
        ("or", Written::Inside),
    ] {
        let (t, w, then, more) = entry_words(&signed_out(&[label]));
        assert_eq!(
            (&t, &w),
            (&title, &why),
            "{label}: named outside the action"
        );
        assert!(
            then.ends_with(label),
            "{label}: the action ends in it: {then}"
        );
        assert!(more.iter().any(|l| l.contains(label)), "{label}: {more:?}");
        // NEGATIVE CONTROL: the word is one the lints know.
        let (reason, step) = match where_it_faults {
            Written::Reason => (
                format!("{label} is signed out, and only a browser sign-in restores it"),
                then.clone(),
            ),
            Written::Leading => (why.clone(), format!("{label} in with aterm harness")),
            Written::Inside => (why.clone(), format!("Sign in {label} under Details")),
        };
        let fault = Entry::new(
            Family::Accounts,
            "",
            "signed-out",
            &title,
            Why::OnlyYou(Reason::new(reason)),
            Then::step(step),
        )
        .message()
        .err();
        assert!(
            matches!(fault, Some(EntryFault::Ask(_))),
            "{label} written {where_it_faults:?}: {fault:?}"
        );
    }
}

/// AN AUTOPILOT SET FROM THE WINDOW IS A RECORD (the Autopilot design, §2):
/// titled with the mode the session now runs under, its first line led by
/// the tab and saying what the mode does, its second whose word it is, and
/// keyed per session so the next replaces it. NEGATIVE CONTROL: a session no
/// tab shows leads with nothing.
#[test]
fn an_autopilot_set_from_the_window_is_a_keyed_record() {
    use crate::autopilot::AutopilotView;
    let msg = super::autopilot_set("s-a1", "in tab 2", AutopilotView::Land, true, None);
    assert_eq!(msg.title, "Autopilot set to land");
    assert_eq!(attention(&msg), Ok(Attention::Record));
    assert_eq!(msg.tag, tags::HARNESS);
    assert_eq!(msg.key.as_deref(), Some("harness.autopilot.s-a1"));
    assert_eq!(
        msg.detail,
        [
            "Tab 2: the harness wraps the work up, then stops",
            "this session's own setting"
        ]
    );
    let back = super::autopilot_set("s-a1", "in its tab", AutopilotView::Cruise, false, None);
    assert_eq!(back.title, "Autopilot set to cruise");
    assert_eq!(
        back.detail,
        [
            "the harness keeps the work moving",
            "back to the default, [harness] autopilot"
        ]
    );
}

/// THE AUTOPILOT RECORDS (§10.2.1, §10.2.5; the lifecycle hunt's fire that
/// said "wraps the work up" over every session): every title
/// `autopilot_set`, `autopilot_timer_set` and `autopilot_timer_ended` mint —
/// for every view, a timer today and on another day, each closed-over case
/// and each thing the fire can find — passes the glass title rule; a hand
/// change over an armed timer says it is cancelled, and one over none does
/// not; the fire says the work is wrapped up only over work the harness
/// carries, and over a done or landed task, an off harness, an agent's own
/// switch off, no host or a tab with no agent, that there is nothing to land.
/// NEGATIVE CONTROL: main's two titles break the rule.
#[test]
fn the_autopilot_records_keep_the_title_rule_and_say_what_the_fire_found() {
    use super::FireOver;
    use crate::autopilot::AutopilotView;
    use crate::autopilot_timers::ClosedOver;
    use crate::claude_lights::{Clock, LandAtGlass, Weekday};
    use crate::harness_glass::Stamp;
    use aterm_messages::text::glass_title_fault;
    const TODAY: i64 = 20_007;
    let six = LandAtGlass {
        clock: Clock::of_minute(18 * 60),
        other_day: None,
    };
    let sat = LandAtGlass {
        clock: Clock::of_minute(16 * 60),
        other_day: Some(Weekday::Sat),
    };
    let at_1742 = |day| {
        Some(Stamp {
            clock: Clock::of_minute(17 * 60 + 42),
            day,
        })
    };
    let mut titles = Vec::new();
    for view in AutopilotView::ALL {
        for cancelled in [None, Some(six), Some(sat)] {
            for own in [true, false] {
                let msg = super::autopilot_set("s-1", "in tab 3", view, own, cancelled);
                let cancel = msg
                    .detail
                    .iter()
                    .find(|line| line.ends_with(" landing time is cancelled"));
                match cancelled {
                    Some(at) => assert_eq!(
                        cancel.map(String::as_str),
                        Some(
                            format!(
                                "the {} landing time is cancelled",
                                crate::autopilot_menu::when(at)
                            )
                            .as_str()
                        ),
                        "{view:?}"
                    ),
                    None => assert_eq!(cancel, None, "{view:?}: no timer, no cancel line"),
                }
                titles.push(msg.title);
            }
        }
    }
    let set = super::autopilot_timer_set("s-1", "in tab 3", six);
    assert_eq!(set.title, "Autopilot cruises until 18:00");
    assert_eq!(
        set.detail,
        ["Tab 3: the harness keeps the work moving, then wraps it up at that time"]
    );
    let saturday = super::autopilot_timer_set("s-1", "in tab 3", sat);
    assert_eq!(saturday.title, "Autopilot cruises until Sat 16:00");
    titles.push(set.title);
    titles.push(saturday.title);
    let ended = |clock, passed, over| {
        super::autopilot_timer_ended("s-1", "in tab 3", clock, passed, over, TODAY)
    };
    let turned = "Tab 3: at 18:00 the word turned land; ";
    for (over, then) in [
        (
            FireOver::Work,
            "the harness wraps the work up at its next point, then stops".to_string(),
        ),
        (
            FireOver::Done(at_1742(TODAY)),
            "nothing to land, the task was done at 17:42".to_string(),
        ),
        (
            FireOver::Done(at_1742(TODAY - 1)),
            format!(
                "nothing to land, the task was done at {} 17:42",
                Weekday::of_days(TODAY - 1).short()
            ),
        ),
        (
            FireOver::Done(None),
            "nothing to land, the task was done".to_string(),
        ),
        (
            FireOver::Landed(at_1742(TODAY)),
            "nothing to land, the task landed at 17:42".to_string(),
        ),
        (
            FireOver::MasterOff,
            "the harness is off in Settings, so nothing is landed".to_string(),
        ),
        (
            FireOver::AgentOff(aterm_phase::Program::Claude),
            "Supervise Claude Code sessions is off, so nothing is landed".to_string(),
        ),
        (
            FireOver::Unhosted,
            "this aterm has no control socket, so nothing is landed".to_string(),
        ),
        (
            FireOver::NoAgent,
            "no agent runs in the tab, so there is nothing to land".to_string(),
        ),
    ] {
        let msg = ended(Some("18:00"), None, over);
        assert_eq!(msg.title, "Autopilot began landing", "{over:?}");
        assert_eq!(msg.detail, [format!("{turned}{then}")], "{over:?}");
        assert_eq!(
            then.contains("wraps the work up"),
            over == FireOver::Work,
            "{over:?}: only work the harness carries is wrapped up"
        );
        assert_eq!(attention(&msg), Ok(Attention::Record), "{over:?}");
        assert_eq!(msg.key.as_deref(), Some("harness.autopilot.s-1"));
        titles.push(msg.title);
    }
    for (passed, line) in [
        (
            ClosedOver::Aterm,
            "Tab 3: the 18:00 landing time passed while aterm was closed; land is on",
        ),
        (
            ClosedOver::Tab,
            "Tab 3: the 18:00 landing time passed while the tab was closed; land is on",
        ),
    ] {
        let msg = ended(Some("18:00"), Some(passed), FireOver::Done(at_1742(TODAY)));
        assert_eq!(
            msg.detail,
            [line],
            "{passed:?}: the closed-over line stands"
        );
        titles.push(msg.title);
    }
    let unzoned = ended(None, None, FireOver::Work);
    assert_eq!(
        unzoned.detail,
        [
            "Tab 3: the word turned land; the harness wraps the work up at its next point, then stops"
        ]
    );
    titles.push(unzoned.title);
    for title in &titles {
        assert_eq!(glass_title_fault(title), None, "{title}");
    }
    // NEGATIVE CONTROL: main's titles of the timer's two records.
    assert!(glass_title_fault("Autopilot set to cruise until 18:00, then land").is_some());
    assert!(glass_title_fault("autopilot: cruise until 18:00 ended; land").is_some());
}

/// ONLY AN INTERRUPT STANDS ON THE BAND (the owner, 2026-09-28: "there needs
/// to be some clear reason why the user must be engaged and what the user
/// should do ... don't continue to persist alerts to the user that are not
/// actionable"; the second reviews of 2026-09-29: this claim was type-checked
/// and never observed). For a roster of rows that covers every class the
/// classifier has, the reporter's entry is a standing warn row exactly where
/// [`Row::interrupt`] names a need and says that need's words; every other
/// row — a wait before or after its notice, a usage limit, a rest, a
/// refusal, a give-up after the owner's own press three days behind (round
/// six, F7: it stood as a warn row for days) — is a record (`LogOnly`).
/// The pure half of the pre-landing drive (`plan-upgrade` 12.0): the host's
/// half is driven against a private instance.
#[test]
fn only_an_interrupt_stands_on_the_band() {
    use aterm_agent::harness::upgrade::{Agent, GAVE_UP, Phase};
    use aterm_agent::harness::upgrade_drive::{Attention, HeldBy, Row};
    const NOW: u64 = 1_790_311_076;
    const DAY: u64 = 86_400;
    let held = |n: u32| -> Vec<HeldBy> {
        (0..n)
            .map(|i| HeldBy {
                pid: 4_000 + i,
                name: "zsh".into(),
                since: NOW - 3_600,
            })
            .collect()
    };
    let base = |agent: Agent| Row {
        session: "07422bc3-1be9-4875-9672-2eaf7b28aa21".into(),
        tab: "s-3e79b5bde172ad0088ad".into(),
        from: "2.1.283".into(),
        to: "2.1.284".into(),
        source: "managed".into(),
        agent,
        behind_since: NOW - 3 * DAY,
        minted_at: NOW - 3 * DAY,
        ..Row::default()
    };
    let mut roster: Vec<(&str, Row)> = Vec::new();
    for agent in [Agent::Claude, Agent::Codex] {
        let b = base(agent);
        roster.push((
            "a pending round nothing has visited",
            Row {
                phase: Phase::Pending,
                ..b.clone()
            },
        ));
        for wait in [
            "settling",
            "attended",
            "not-idle:busy",
            "background",
            "awaiting-ready",
            "announce-refused:changed",
            "announce-refused:held",
            "draft",
            "box",
            "limited",
            "login",
            "queued",
        ] {
            for phase in [
                Phase::Pending,
                Phase::Announced {
                    at_s: NOW - 60,
                    asks: 2,
                },
            ] {
                for age in [3_600, 30 * 3_600, 9 * DAY] {
                    roster.push((
                        wait,
                        Row {
                            phase: phase.clone(),
                            wait: wait.into(),
                            wait_since: NOW - age,
                            behind_since: NOW - age,
                            minted_at: NOW - age,
                            held_by: held(16),
                            own_work_at: NOW - 60,
                            ..b.clone()
                        },
                    ));
                }
            }
        }
        roster.push((
            "a give-up after the owner's press, three days behind",
            Row {
                phase: Phase::Failed(GAVE_UP.into()),
                streak_why: GAVE_UP.into(),
                stop_streak: 1,
                wait: "failed".into(),
                ..b.clone()
            },
        ));
        roster.push((
            "a round that gave up and rests",
            Row {
                phase: Phase::Failed("unanswered".into()),
                streak_why: "unanswered".into(),
                stop_streak: 2,
                wait: "failed".into(),
                held_by: held(16),
                own_work_at: NOW - 60,
                ..b.clone()
            },
        ));
        roster.push((
            "a refusal",
            Row {
                phase: Phase::Failed("not-a-shell-job".into()),
                ..b.clone()
            },
        ));
        roster.push((
            "a suspended agent",
            Row {
                phase: Phase::Pending,
                wait: "not-foreground".into(),
                wait_since: NOW - 2 * 3_600,
                ..b.clone()
            },
        ));
        // The interrupts.
        roster.push((
            "the agent is gone",
            Row {
                phase: Phase::Failed("stale-exit".into()),
                exited_at: NOW - 600,
                ..b.clone()
            },
        ));
        roster.push((
            "a shutdown that hung",
            Row {
                phase: Phase::Exiting {
                    at_s: NOW - 20 * 60,
                },
                ..b.clone()
            },
        ));
        roster.push((
            "a relaunch that never reached a prompt",
            Row {
                phase: Phase::Relaunched {
                    at_s: NOW - 40 * 60,
                },
                exited_at: NOW - 41 * 60,
                ..b.clone()
            },
        ));
    }
    let mut standing = 0;
    for (what, row) in &roster {
        let msg = super::agent_upgrade_stalled(row, NOW, "in tab 2");
        let need = row.interrupt(NOW);
        let is_row = matches!(msg.hold, Hold::STANDING);
        assert_eq!(
            is_row,
            need.is_some(),
            "{what} ({:?}, {:?}): {:?} / {:?}",
            row.agent,
            row.wait,
            msg.hold,
            row.attention(NOW).level()
        );
        if let Some(need) = need {
            standing += 1;
            assert_eq!(msg.severity, Severity::Warn, "{what}");
            assert_eq!(msg.detail[0], need.why, "{what}");
            assert_eq!(msg.detail[1], need.then, "{what}");
        } else {
            assert!(
                matches!(msg.hold, Hold::LogOnly),
                "{what}: a wait is a record: {:?}",
                msg.hold
            );
            assert!(
                !matches!(row.attention(NOW), Attention::Interrupt(_)),
                "{what}"
            );
        }
    }
    assert!(standing >= 6, "the roster holds the interrupts: {standing}");
}

/// RULING 283, THE AUDIT'S REAL CASE: a round that gave up, is resting,
/// and is more than six hours behind asks again on its own, so it is a
/// RECORD with no tab mark — and it offers `Upgrade now`, which re-arms it
/// at once. Ruling 314: the why AND the next step are the sentence
/// Settings ▸ Messages sets for a person; a refused press restated above
/// them joins the sentence; a record with no move line, or no tab's key,
/// keeps one line.
#[test]
fn a_resting_round_that_gave_up_is_a_record_that_offers_upgrade_now() {
    use aterm_agent::harness::upgrade::Phase;
    use aterm_agent::harness::upgrade_drive::Row;
    use aterm_messages::UpgradeWord;
    const NOW: u64 = 1_790_311_076;
    let resting = Row {
        tab: "s-b5cf2faabac5ce5127bd".into(),
        from: "2.1.281".into(),
        to: "2.1.282".into(),
        phase: Phase::Failed("unanswered".into()),
        wait: "failed".into(),
        behind_since: NOW - 8 * 3_600,
        retry_at: NOW + 3_000,
        stop_streak: 1,
        streak_why: "unanswered".into(),
        ..Row::default()
    };
    let msg = super::agent_upgrade_stalled(&resting, NOW, "in tab 2");
    assert_eq!((msg.severity, msg.hold), (Severity::Info, Hold::LogOnly));
    assert!(
        msg.actions.iter().any(|a| matches!(
            a,
            Intent::AgentUpgrade {
                word: UpgradeWord::Now,
                ..
            }
        )),
        "{:?}",
        msg.actions
    );
    let key = msg.key.as_deref();
    assert_eq!(
        super::sentence_lines(key, &msg.detail),
        2,
        "{:?}",
        msg.detail
    );
    let mut refused = msg.detail.clone();
    refused.insert(0, "the upgrade no longer takes that word".to_string());
    assert_eq!(super::sentence_lines(key, &refused), 3);
    assert_eq!(super::sentence_lines(None, &msg.detail), 1, "no key");
    assert_eq!(
        super::sentence_lines(Some(KEY_AGENT_UPGRADE), &msg.detail),
        1
    );
    assert_eq!(
        super::sentence_lines(key, &msg.detail[..2]),
        1,
        "no move line"
    );
}

/// A TAB NO WORKER OF ATERM'S RUNS IN, BY SOMEONE ELSE'S HAND, IS NEVER
/// SAID AS ATERM'S OWN FAULT (the review of 2026-09-30, of C10): its agent
/// suspended with `^Z` (not in front), its tab under another supervisor's
/// claim, or its Autopilot turned off by the owner (the review of a295792e4)
/// (`Row::aside`, which the owner's view sets). The record nothing has
/// stamped for three hours is told by what holds it — "it is not in front in
/// its tab", "another supervisor holds its tab", "its autopilot is off" —
/// and by what moves it: `fg` in the tab, that supervisor letting go, the
/// Autopilot on again. NEGATIVE CONTROL: the same record with no aside reads
/// as aterm's own fault ("not checked for 3 h — aterm's own fault"), as it
/// must where aterm's watch is at fault.
#[test]
fn a_tab_held_aside_is_said_by_whose_hand_holds_it() {
    use aterm_agent::harness::upgrade::Phase;
    use aterm_agent::harness::upgrade_drive::{AUTOPILOT_OFF, CLAIMED, NOT_FOREGROUND, Row};
    const NOW: u64 = 1_790_311_076;
    let row = |aside: &str| Row {
        tab: "s-b5cf2faabac5ce5127bd".into(),
        from: "2.1.281".into(),
        to: "2.1.282".into(),
        phase: Phase::Pending,
        wait: if aside.is_empty() {
            "not-idle:busy"
        } else {
            aside
        }
        .into(),
        aside: aside.into(),
        behind_since: NOW - 7 * 3_600,
        watch_at: NOW - 2 * 3_600,
        looked_at: NOW - 3 * 3_600,
        awake_since: NOW - 86_400,
        ..Row::default()
    };
    let control = super::agent_upgrade_stalled(&row(""), NOW, "in tab 2");
    assert!(
        control.detail[0].ends_with("aterm's own fault"),
        "NEGATIVE CONTROL: {:?}",
        control.detail
    );
    for (aside, what, moves) in [
        (
            NOT_FOREGROUND,
            "it is not in front in its tab",
            "it moves once it is back in front in its tab (fg there); Skip version keeps it \
             on 2.1.281",
        ),
        (
            CLAIMED,
            "another supervisor holds its tab",
            "it moves once the supervisor holding its tab lets it go; Skip version keeps it \
             on 2.1.281",
        ),
        (
            AUTOPILOT_OFF,
            "its autopilot is off",
            "it moves once its autopilot is cruise or land again; Skip version keeps it on \
             2.1.281",
        ),
    ] {
        let msg = super::agent_upgrade_stalled(&row(aside), NOW, "in tab 2");
        assert_eq!((msg.severity, msg.hold), (Severity::Info, Hold::LogOnly));
        assert!(msg.detail[0].ends_with(what), "{aside}: {:?}", msg.detail);
        assert_eq!(msg.detail[1], moves, "{aside}");
        assert!(
            msg.detail.iter().all(|l| !l.contains("fault")),
            "{aside}: {:?}",
            msg.detail
        );
    }
}

/// THE OWNER'S `Upgrade now` IS ANSWERED ON THE GLASS (2026-09-28: "when I
/// pressed 'update' in the claude version update button, nothing seemed
/// to happen?"). The press answers its row in place, and `--now` quiets
/// the stall for `NOW_QUIETS_S`; after that the host posts the tab's
/// stall again, and a move waiting on the agent's own work — which
/// `--now` does not waive — was a record posted and folded in the same
/// millisecond (messages.log, ids 64 and 66), withdrawing the answered
/// row under the same key. The owner's word still stood; nothing on the
/// glass said so. While it stands the answer is a calm notice that says
/// what it waits on and folds after [`ANSWER_MAX`] (the owner's rule: the
/// answer to their own press, with no action left in it, does not
/// persist); with no word it stays a record (D5).
///
/// FAILS WITHOUT THE FIX: the hurried row is `LogOnly`, as the unasked
/// one is.
#[test]
fn the_owners_upgrade_now_is_answered_on_the_glass() {
    use aterm_agent::harness::upgrade::{Phase, Request};
    use aterm_agent::harness::upgrade_drive::Row;
    const NOW: u64 = 1_790_311_076;
    let row = |request: Request| Row {
        tab: "s-b5cf2faabac5ce5127bd".into(),
        from: "2.1.280".into(),
        to: "2.1.284".into(),
        phase: Phase::Announced {
            at_s: NOW - 600,
            asks: 3,
        },
        wait: "background".into(),
        // Inside the move clock (`MOVE_BUDGET_S`): past it the round no
        // longer "asks on its own" (its words say so), and the answer to
        // the owner's press is the same (below).
        behind_since: NOW - 8 * 3_600,
        request,
        // Pressed longer ago than `--now` quiets a stall
        // (`NOW_QUIETS_S`, which is `REASK_S`): the stall is back, and so is
        // the host's row.
        request_at: NOW - aterm_agent::harness::upgrade::REASK_S - 60,
        ..Row::default()
    };
    let hurried = row(Request::Now);
    assert!(hurried.asks_on_its_own(NOW), "still the upgrade working");
    let msg = super::agent_upgrade_stalled(&hurried, NOW, "in tab 1");
    assert_eq!(
        (msg.severity, msg.hold),
        (Severity::Info, Hold::For(super::ANSWER_MAX))
    );
    assert!(msg.actions.is_empty(), "the answer is no new decision");
    // NEGATIVE CONTROL: no word from the owner, the same wait — a record.
    let msg = super::agent_upgrade_stalled(&row(Request::None), NOW, "in tab 1");
    assert_eq!((msg.severity, msg.hold), (Severity::Info, Hold::LogOnly));
}

use super::*;

fn sentences(msg: &Message) -> Vec<String> {
    std::iter::once(msg.title.clone())
        .chain(msg.detail.iter().cloned())
        .collect()
}

fn every_debugger_access_record() -> Vec<Message> {
    use crate::debugger_access::DebuggerRecord;
    [
        DebuggerRecord::On,
        DebuggerRecord::OnOutside,
        DebuggerRecord::Off,
        DebuggerRecord::WasOnNowOff,
    ]
    .into_iter()
    .map(debugger_access_record)
    .collect()
}

/// DEBUGGER ACCESS RECORDS (design §3.8): records, never rows, with the
/// undo — the command that turns access back off — on every "on" record
/// and only those.
#[test]
fn the_debugger_access_records_are_records_with_the_undo_only_when_on() {
    use crate::debugger_access::DebuggerRecord;
    for record in [
        DebuggerRecord::On,
        DebuggerRecord::OnOutside,
        DebuggerRecord::Off,
        DebuggerRecord::WasOnNowOff,
    ] {
        let msg = debugger_access_record(record);
        assert_eq!(msg.hold, Hold::LogOnly, "{msg:?}");
        assert_eq!(
            msg.detail
                .iter()
                .any(|l| l.contains(aterm_containment::consent::DEBUGGER_ACCESS_REVERT)),
            matches!(record, DebuggerRecord::On | DebuggerRecord::OnOutside),
            "the undo rides every on record, and only those: {msg:?}"
        );
    }
}

/// Every message a launch or reload can post, from fixtures that stand
/// in for the real producers' words.
fn every_reporter_message() -> Vec<Message> {
    let mut warns = ConfigWarnings::default();
    warns.push(
        ConfigFamily::Keybindings,
        "config keybindings: skipping \"ctrl+x\": unknown action \"foo\"".into(),
    );
    warns.push(
        ConfigFamily::Keybindings,
        "config key_sequences: skipping \"ctrl+a b\": bad chord".into(),
    );
    warns.push(
        ConfigFamily::IgnoredKeys,
        "config line 3: unknown key \"windw_padding\" — did you mean \"window_padding\"?".into(),
    );
    warns.push(
        ConfigFamily::Restart,
        "columns/lines applies on next launch (resize the window to change size now)".into(),
    );
    warns.push(
        ConfigFamily::Restart,
        "gpu applies on next launch (the renderer backend is chosen at startup)".into(),
    );
    warns.push(
        ConfigFamily::Fonts,
        "config font_family_bold: \"Nope\" is not an admissible font (not found); ignored".into(),
    );
    crate::app_config::collect_key_notices(
        &mut warns,
        "show_hud = true\n[packages]\nenabled = true\n\
         auto_update = false\nseed_install = true\n",
    );
    let mut all = warns.into_messages();
    // Every family's title, singular and plural, so a re-word of any of
    // them meets the attention gate (audit 2026-09-24).
    for family in [
        ConfigFamily::Keybindings,
        ConfigFamily::IgnoredKeys,
        ConfigFamily::RetiredKeys,
        ConfigFamily::UnacceptedValues,
        ConfigFamily::CursorTrail,
        ConfigFamily::Fonts,
        ConfigFamily::Assets,
        ConfigFamily::Restart,
        ConfigFamily::SecureKeyboard,
    ] {
        for n in [1, 3] {
            let mut one = ConfigWarnings::default();
            for i in 0..n {
                one.push(family, format!("config sentence {i} for this family"));
            }
            all.extend(one.into_messages());
        }
    }
    all.push(crash_message(&crate::logging::CrashEvidence {
        path: std::path::PathBuf::from("/Users/_an/Library/Logs/aterm/crash-1-1.log.seen"),
        head: vec!["aterm-gui 0.1.0 crashed at unix 1.000".into()],
    }));
    all.push(killed_message(&crate::logging::KillEvidence {
        marker: std::path::PathBuf::from(
            "/Users/_an/Library/Logs/aterm/crash-marker-1-1-app.log.seen",
        ),
        log: std::path::PathBuf::from("/Users/_an/Library/Logs/aterm/aterm.log"),
    }));
    for (cause, fix) in [
        ("launched with the control socket off", None),
        (
            "no per-user runtime folder for a control socket",
            Some(SOCKET_DIR_UNSET_FIX),
        ),
        (
            "/run/user/501/aterm can't be made private to you for a control socket",
            Some(SOCKET_DIR_UNUSABLE_FIX),
        ),
        (
            "~/Library/Application Support/aterm can't be made private to you for a control \
             socket",
            Some(SOCKET_DIR_UNUSABLE_FIX),
        ),
        (
            "the --control-sock path is 120 bytes, over the 103-byte limit",
            Some("use a shorter path"),
        ),
    ] {
        all.push(harness_without_socket(cause, fix));
    }
    for class in [
        crate::crash_journal::DeathClass::Killed,
        crate::crash_journal::DeathClass::Signal,
        crate::crash_journal::DeathClass::Panic,
    ] {
        for reopened in [
            reopened_fixture(class),
            reopened_with_programs(class, &[]),
            reopened_with_programs(class, &[(1, "vim")]),
            reopened_with_programs(class, &[(0, "claude"), (1, "")]),
            reopened_with_agent(class, &[]),
            reopened_with_agent(class, &[(0, "vim")]),
        ] {
            for relaunching in [false, true] {
                all.push(journal_reopened_message(
                    &reopened,
                    None,
                    None,
                    Some(std::path::Path::new(
                        "/Users/_an/Library/Logs/aterm/aterm.log",
                    )),
                    relaunching,
                    PanesWithoutFolder::NONE,
                    0,
                ));
            }
        }
    }
    // A pane whose folder no shell could start in (audit #7 finding 48):
    // the reopened line that counts it, and each fault's row; and a pane
    // a program held whose folder is not here (audit #8).
    for (gone, unasked) in [(1, 0), (2, 0), (0, 1)] {
        all.push(journal_reopened_message(
            &reopened_fixture(crate::crash_journal::DeathClass::Killed),
            None,
            None,
            None,
            false,
            PanesWithoutFolder { gone, unasked },
            0,
        ));
    }
    for class in [
        crate::crash_journal::DeathClass::Killed,
        crate::crash_journal::DeathClass::Signal,
    ] {
        for reopened in [
            reopened_with_programs(class, &[]),
            reopened_with_agent(class, &[]),
        ] {
            all.push(journal_reopened_message(
                &reopened,
                None,
                None,
                None,
                true,
                PanesWithoutFolder {
                    gone: 1,
                    unasked: 0,
                },
                0,
            ));
        }
    }
    for fault in crate::spawn_folder::Fault::ALL {
        all.push(folder_row(fault, &named(&["/Users/_an/src/old"])));
        all.push(folder_row(fault, &named(&["/a", "/b"])));
    }
    for note in journal_note_fixtures() {
        all.push(journal_note_message(
            &note,
            Some(std::path::Path::new(
                "/Users/_an/Library/Logs/aterm/aterm.log",
            )),
        ));
    }
    {
        use aterm_agent::harness::relaunch::Outcome;
        use aterm_agent::harness::upgrade::Agent;
        let missed = [
            (
                "in tab 2".to_string(),
                Agent::Claude,
                Outcome::Cannot("shell-gone".into()),
            ),
            (
                "in tab 3".to_string(),
                Agent::Codex,
                Outcome::NotYet(String::new()),
            ),
        ];
        all.push(restored_agents_not_resumed(
            &missed[..1],
            Some(std::path::Path::new(
                "/Users/_an/Library/Logs/aterm/aterm.log",
            )),
        ));
        all.push(restored_agents_not_resumed(&missed, None));
    }
    all.push(launch_load_failure(
        "aterm.toml is not a valid configuration (expected `=` at line 3) — every \
         setting is running at its default. Fix /home/ana/.config/aterm.toml and it loads on \
         the next change.",
    ));
    all.push(cpu_renderer_no_effect(
        "background_opacity",
        "it has no translucent present path, so the window stays solid",
    ));
    all.push(backdrop_declined(
        "background_material is styling the title bar only",
        "this display stack refused the DirectComposition backdrop swapchain",
    ));
    all.push(gpu_lost());
    all.extend(integration_lost_fixtures());
    all.push(integration_rekey_waits("in tab 2", "s-0123456789abcdef"));
    for place in ["in tab 2", "in window 12, tab 10", "in its tab"] {
        all.push(resume_ask(
            "s-0123456789abcdef",
            &UpgradeSpot {
                place: place.to_string(),
                nav: None,
            },
            &Exception {
                kind: ExceptionKind::Login,
                when: Urgency::Now,
                why: "Its login is gone, and the roster has no other account to move it to."
                    .to_string(),
                action: "Run aterm harness accounts login pool-a.".to_string(),
                clears: "the login signed in again, an account to move to, or the agent working",
                escalated: false,
            },
        ));
    }
    all.push(a11y_publisher_dead("Server GUID mismatch", true));
    all.push(a11y_publisher_dead("Server GUID mismatch", false));
    all.push(presence_not_saved(
        "presence.band",
        "Presence Band",
        "the write was refused",
        false,
    ));
    all.push(presence_not_saved(
        "presence.rim",
        "Presence Rim",
        "the write could not be verified",
        true,
    ));
    all.push(theme_not_saved(
        "Gruvbox Dark",
        "aterm.toml changed first; its current value was kept",
        false,
    ));
    all.push(theme_not_saved(
        "Gruvbox Dark",
        "the write could not be verified",
        true,
    ));
    all.push(fleet_hold(true, "main%20broken"));
    all.push(fleet_hold(false, "main%20broken"));
    all.push(fleet_hold(false, crate::fabric::FABRIC_LOST));
    for view in crate::autopilot::AutopilotView::ALL {
        all.push(autopilot_set("s-1", "in tab 2", view, true, None));
        all.push(autopilot_set("s-1", "in its tab", view, false, None));
    }
    all.push(hold_refused(true, "no session"));
    all.push(hold_refused(false, "no session"));
    all.push(fabric_failure(
        "document",
        "Couldn't open the document",
        &["no such file", "~/x.md"],
    ));
    all.push(fabric_failure(
        "on",
        "Couldn't start Fabric On",
        &["cannot spawn its thread"],
    ));
    all.push(fabric_failure(
        "inbox",
        "Couldn't open the inbox",
        &["no such file"],
    ));
    all.push(fabric_failure(
        "status",
        "Couldn't start Fabric Status",
        &["cannot spawn its thread"],
    ));
    all.push(fabric_status("on", "Fabric On finished", "", true));
    all.push(fabric_status(
        "off",
        "Couldn't run Fabric Off",
        "exited 2",
        false,
    ));
    all.push(serious_mode_feedback(
        "Serious Mode was not changed: aterm.toml changed first",
    ));
    all.push(serious_mode_feedback(
        "Serious Mode was saved but could not be applied: x",
    ));
    all.push(serious_mode_feedback(
        "Serious Mode was saved, but a newer aterm.toml edit now controls it",
    ));
    all.push(serious_mode_feedback(
        "Serious Mode may have been written but could not be verified; reload before \
         retrying: the disk generation moved",
    ));
    for head in [
        "Config observation was not valid TOML: expected `]`",
        "Config reconciliation failed; queued changes were not written: io",
        "Manual saved aterm.toml, but its exact generation could not be admitted: x",
        "Config publication could not be verified: x",
        "Config was NOT saved: x",
        "Something new went wrong: x",
    ] {
        all.push(config_lane_error(head));
    }
    // The live agent upgrade's three (gap audit 2026-09-24): waiting (a
    // record), stalled (a row), done (a record).
    all.extend(agent_upgrade_waiting(
        "Claude Code",
        &["2.1.282", "2.1.282"],
    ));
    all.extend(agent_upgrade_waiting("Claude Code", &["2.1.282"]));
    all.extend(agent_upgrade_waiting(
        "Claude Code",
        &["2.1.282", "2.1.283"],
    ));
    all.extend(agent_upgrade_waiting("Codex", &["0.157.1"]));
    // One session's record (ruling 380), by its tab and in a second
    // window, Claude Code's longer product name included.
    for (agent, from, to) in [
        (
            aterm_agent::harness::upgrade::Agent::Codex,
            "0.157.1",
            "0.158.0",
        ),
        (
            aterm_agent::harness::upgrade::Agent::Claude,
            "2.1.282",
            "2.1.283",
        ),
    ] {
        for place in ["in tab 1", "in window 2, tab 11"] {
            all.push(agent_upgrade_waiting_in(
                &aterm_agent::harness::upgrade_drive::Row {
                    tab: "s-09205e59a0bbd30464d4".into(),
                    from: from.into(),
                    to: to.into(),
                    agent,
                    phase: aterm_agent::harness::upgrade::Phase::Pending,
                    behind_since: 1_790_572_868,
                    ..Default::default()
                },
                place,
            ));
        }
    }
    let upgrade = aterm_agent::harness::upgrade_drive::Row {
        tab: "s-b5cf2faabac5ce5127bd".into(),
        from: "2.1.281".into(),
        to: "2.1.282".into(),
        phase: aterm_agent::harness::upgrade::Phase::Failed("unanswered".into()),
        outcome: "claude restarted on 2.1.282 · model claude-opus-5-5".into(),
        ..Default::default()
    };
    all.push(agent_upgrade_stalled(&upgrade, 1_790_311_076, "in tab 2"));
    all.push(agent_upgrade_stalled(
        &aterm_agent::harness::upgrade_drive::Row {
            phase: aterm_agent::harness::upgrade::Phase::Pending,
            behind_since: 1_790_280_544,
            wait: "not-idle:busy".into(),
            ..upgrade.clone()
        },
        1_790_311_076,
        "in tab 2",
    ));
    // A Claude Code behind its own running work (2026-09-26), after a notice
    // and before one: the sentence guards read both remedy lines.
    for (phase, wait) in [
        (
            aterm_agent::harness::upgrade::Phase::Announced {
                at_s: 1_790_300_000,
                asks: 2,
            },
            "background",
        ),
        (
            aterm_agent::harness::upgrade::Phase::Pending,
            "not-idle:shell",
        ),
    ] {
        all.push(agent_upgrade_stalled(
            &aterm_agent::harness::upgrade_drive::Row {
                phase,
                behind_since: 1_790_280_544,
                wait: wait.into(),
                ..upgrade.clone()
            },
            1_790_311_076,
            "in tab 2",
        ));
    }
    // A record nothing has checked (design record C10): aterm's own
    // fault, said as such — the sentence guards read its remedy too.
    all.push(agent_upgrade_stalled(
        &aterm_agent::harness::upgrade_drive::Row {
            phase: aterm_agent::harness::upgrade::Phase::Pending,
            behind_since: 1_790_307_476,
            wait: "not-idle:busy".into(),
            watch_at: 1_790_311_076 - 2 * 3_600,
            looked_at: 1_790_311_076 - 3 * 3_600,
            awake_since: 1_790_311_076 - 86_400,
            ..upgrade.clone()
        },
        1_790_311_076,
        "in tab 2",
    ));
    all.push(agent_upgrade_done(&upgrade, "in tab 2"));
    // THE ROWS THAT STAND (ruling 423: the agent the upgrade ended, a
    // shutdown that hung, a relaunch that reached no prompt), the answer to
    // the owner's own `Upgrade now`, and aterm's own fault: every sentence
    // they say is read by the title, restart and glass-row guards.
    {
        use aterm_agent::harness::upgrade::{Phase, Request};
        const NOW: u64 = 1_790_311_076;
        for row in [
            aterm_agent::harness::upgrade_drive::Row {
                phase: Phase::Failed("stale-exit".into()),
                exited_at: NOW - 600,
                ..upgrade.clone()
            },
            aterm_agent::harness::upgrade_drive::Row {
                phase: Phase::Exiting { at_s: NOW - 1_200 },
                ..upgrade.clone()
            },
            aterm_agent::harness::upgrade_drive::Row {
                phase: Phase::Relaunched { at_s: NOW - 2_400 },
                ..upgrade.clone()
            },
            aterm_agent::harness::upgrade_drive::Row {
                phase: Phase::Announced {
                    at_s: NOW - 600,
                    asks: 2,
                },
                wait: "background".into(),
                request: Request::Now,
                request_at: NOW - 30,
                behind_since: NOW - 32 * 60,
                ..upgrade.clone()
            },
        ] {
            all.push(agent_upgrade_stalled(&row, NOW, "in tab 2"));
        }
        let unclassified = aterm_agent::harness::upgrade_drive::Row {
            phase: Phase::Pending,
            wait: "zzz-unclassified".into(),
            wait_since: NOW - 2 * 3_600,
            behind_since: NOW - 3 * 3_600,
            ..upgrade.clone()
        };
        if let aterm_agent::harness::upgrade_drive::Attention::Fault(need) =
            unclassified.attention(NOW)
        {
            all.push(agent_upgrade_fault(&unclassified, &need, "in tab 2"));
        }
    }
    // WHERE THE OWNER HAS MORE THAN ONE WINDOW the place names it (`in
    // window 2, tab 1`), a word longer: every title that names the tab —
    // a wait `Upgrade now` moves, one it does not (`… yet`), a stop, and
    // each refused word — still keeps the glass title rule, at two-digit
    // windows and tabs too.
    for place in [tab_place(Some(2), 1), tab_place(Some(12), 10)] {
        for (phase, wait) in [
            (
                aterm_agent::harness::upgrade::Phase::Pending,
                "not-idle:busy",
            ),
            (
                aterm_agent::harness::upgrade::Phase::Pending,
                "not-idle:shell",
            ),
            (
                aterm_agent::harness::upgrade::Phase::Announced {
                    at_s: 1_790_300_000,
                    asks: 2,
                },
                "background",
            ),
            (
                aterm_agent::harness::upgrade::Phase::Failed("not-a-shell-job".into()),
                "",
            ),
        ] {
            all.push(agent_upgrade_stalled(
                &aterm_agent::harness::upgrade_drive::Row {
                    phase,
                    wait: wait.into(),
                    behind_since: 1_790_280_544,
                    ..upgrade.clone()
                },
                1_790_311_076,
                &place,
            ));
        }
        for word in aterm_messages::UpgradeWord::ALL {
            all.push(agent_upgrade_word_refused(
                word,
                aterm_agent::harness::upgrade::Agent::Claude,
                ("s-1", "2.1.282"),
                "busy:another-sweep",
                &place,
            ));
        }
        all.push(agent_upgrade_done(&upgrade, &place));
    }
    // A stall's end, recorded under its row's key once the row was down
    // (review of 2026-09-27).
    let stalled = agent_upgrade_stalled(&upgrade, 1_790_311_076, "in tab 2");
    for end in [StallEnd::AsksAgain, StallEnd::MovesAgain, StallEnd::Left] {
        all.push(agent_upgrade_stall_over(
            upgrade.agent,
            stalled.key.as_deref().expect("keyed"),
            &stalled.detail,
            "in tab 2",
            end,
        ));
    }
    // The owner's word from the band (gap #21): what each word did, and
    // each kind of refusal.
    all.push(agent_upgrade_worded(
        &upgrade,
        aterm_messages::UpgradeWord::Now,
        "in tab 2",
        true,
    ));
    for word in aterm_messages::UpgradeWord::ALL {
        all.push(agent_upgrade_worded(&upgrade, word, "in tab 2", false));
        for why in [
            "busy:another-sweep",
            "stale:2.1.283",
            "the upgrade in tab s-b5cf2faabac5ce5127bd stopped for good (no-resume): `--now` \
             re-arms only one that gave up",
            // Ruling 283: a round that stopped between the offer and the
            // press, as the writer refuses it now (typed).
            "stopped:1790318276:the upgrade in tab s-b5cf2faabac5ce5127bd stopped \
             (no-resume): `--now` re-arms only one that gave up",
            "stopped:0:the upgrade in tab s-b5cf2faabac5ce5127bd stopped (signal-refused): \
             `--now` re-arms only one that gave up",
        ] {
            all.push(agent_upgrade_word_refused(
                word,
                aterm_agent::harness::upgrade::Agent::Claude,
                ("s-b5cf2faabac5ce5127bd", "2.1.282"),
                why,
                "in tab 2",
            ));
        }
    }
    // The Codex lane's (a daemon-mode client moved, one that named no
    // thread as it exited): its words pass every reporter guard too.
    let codex = aterm_agent::harness::upgrade_drive::Row {
        agent: aterm_agent::harness::upgrade::Agent::Codex,
        session: "codex-s-b5cf2faabac5ce5127bd".into(),
        from: "0.157.0".into(),
        to: "0.157.1".into(),
        phase: aterm_agent::harness::upgrade::Phase::Done,
        outcome: "codex on 0.157.1 · the same conversation resumed; its work never \
                  stopped, in the daemon"
            .into(),
        ..upgrade.clone()
    };
    all.push(agent_upgrade_done(&codex, "in tab 2"));
    all.push(agent_upgrade_stalled(
        &aterm_agent::harness::upgrade_drive::Row {
            phase: aterm_agent::harness::upgrade::Phase::Failed("no-resume-hint".into()),
            ..codex.clone()
        },
        1_790_311_076,
        "in tab 2",
    ));
    // The same, failed after its `/exit` ended the TUI (its record now
    // stands with no holder, and names `codex resume`), and a Codex
    // client whose daemon waits on a detached thread.
    all.push(agent_upgrade_stalled(
        &aterm_agent::harness::upgrade_drive::Row {
            phase: aterm_agent::harness::upgrade::Phase::Failed("no-resume-hint".into()),
            exited_at: 1_790_311_000,
            ..codex.clone()
        },
        1_790_311_076,
        "in tab 2",
    ));
    all.push(agent_upgrade_stalled(
        &aterm_agent::harness::upgrade_drive::Row {
            phase: aterm_agent::harness::upgrade::Phase::Pending,
            behind_since: 1_790_280_544,
            wait: "daemon-first:busy-thread".into(),
            ..codex.clone()
        },
        1_790_311_076,
        "in tab 2",
    ));
    // A Claude Code move that stopped after its SIGTERM ended the agent
    // (it names `claude --resume`), and moves under way that do not move
    // (stuck exiting, exited, relaunched), for both agents (2026-09-27).
    all.push(agent_upgrade_stalled(
        &aterm_agent::harness::upgrade_drive::Row {
            session: "0badf00d-1111-2222-3333-444455556666".into(),
            phase: aterm_agent::harness::upgrade::Phase::Failed("stale-exit".into()),
            exited_at: 1_790_311_000,
            ..upgrade.clone()
        },
        1_790_311_076,
        "in tab 2",
    ));
    for row in [&upgrade, &codex] {
        for (phase, exited_at) in [
            (
                aterm_agent::harness::upgrade::Phase::Exiting {
                    at_s: 1_790_310_000,
                },
                0,
            ),
            (
                aterm_agent::harness::upgrade::Phase::Exiting {
                    at_s: 1_790_310_000,
                },
                1_790_310_010,
            ),
            (
                aterm_agent::harness::upgrade::Phase::Relaunched {
                    at_s: 1_790_310_000,
                },
                1_790_310_010,
            ),
        ] {
            all.push(agent_upgrade_stalled(
                &aterm_agent::harness::upgrade_drive::Row {
                    phase,
                    exited_at,
                    behind_since: 1_790_309_000,
                    ..row.clone()
                },
                1_790_311_076,
                "in tab 2",
            ));
        }
    }
    all.extend(agent_upgrade_waiting(
        "Claude Code and Codex",
        &["2.1.282", "0.157.1"],
    ));
    for (phase, wait) in [
        (
            aterm_agent::harness::upgrade::Phase::Pending,
            "terminal:tmux",
        ),
        (
            aterm_agent::harness::upgrade::Phase::Failed("not-a-shell-job".into()),
            "",
        ),
        (
            aterm_agent::harness::upgrade::Phase::Failed("no-resume".into()),
            "",
        ),
    ] {
        all.push(agent_upgrade_stalled(
            &aterm_agent::harness::upgrade_drive::Row {
                phase,
                wait: wait.into(),
                behind_since: 1_790_311_000,
                ..upgrade.clone()
            },
            1_790_311_076,
            "in tab 2",
        ));
    }
    all.push(log_did_not_open("/x/y.log"));
    all.push(font_family_rejected(
        "font_family: \"Nope\" is not an admissible font",
    ));
    // R14–R22, the retired toast's reporters.
    all.extend(gesture_failures());
    all.push(file_access_question(
        &crate::consent_card::NeedWords::sample(),
    ));
    all.push(file_access_granted());
    all.push(consent_dialog_record(
        "MediaLibrary",
        "the music and media library",
        Some(12),
    ));
    all.push(consent_dialog_record("AddressBook", "your contacts", None));
    all.extend(every_debugger_access_record());
    for posture in [
        aterm_update::which_copy::InstallPosture::MountedImage,
        aterm_update::which_copy::InstallPosture::Translocated,
    ] {
        all.push(install_posture(posture).expect("a posture to fix"));
    }
    for kind in [
        crate::connections::ConnectedSpawnKind::Controlled,
        crate::connections::ConnectedSpawnKind::Controller,
    ] {
        all.push(session_connection_created(
            &crate::connections::first_use_notice_text(kind),
        ));
    }
    all.push(session_connection_created(
        &crate::connections::first_use_connect_notice_text(true, false),
    ));
    all
}

/// The folders a row names, as the fold keeps them.
fn named(dirs: &[&str]) -> crate::spawn_folder::Named {
    crate::spawn_folder::Named {
        dirs: dirs.iter().map(|dir| (*dir).to_owned()).collect(),
        unnamed: 0,
    }
}

/// THE MEMORY CEILING'S WARNING IS A ROW (owner direction 2026-09-29:
/// warn, do not restart): a failure on the glass, keyed per session so the
/// next episode's replaces it, naming the tab by its place and never the
/// raw session id, its resume line kept whole. NEGATIVE CONTROL: the
/// supervisor's information is a record, never a row.
#[test]
fn a_supervisors_memory_warning_is_a_row_and_its_information_a_record() {
    let text = "Claude Code uses 215.1 GB, past aterm's memory ceiling of 46.0 GB; a \
                warning only: nothing is restarted; to restart it yourself: claude --resume \
                0badf00d";
    let row = super::supervisor_warning("s-1", "in tab 3", text);
    assert_eq!(attention(&row), Ok(Attention::Failure));
    assert_ne!(
        super::supervisor_warning("s-2", "in tab 3", text).key,
        row.key,
        "per session"
    );
    let said = row.detail.join(" ");
    assert!(!said.contains("s-1"), "{said}");
    assert!(said.ends_with("claude --resume 0badf00d"), "{said}");
    let note = super::supervisor_note("s-1", "in tab 3", text);
    assert_eq!(attention(&note), Ok(Attention::Record));
}

/// THE MEMORY ROW NEVER CUTS A RESUME LINE
/// ([`aterm_agent::supervise::memory::never_cut`]): a Claude Code launched
/// with many flags, led by its tab, passes the row's 400 characters, so the
/// row names its line with its permission mode alone, whole on one detail
/// line. NEGATIVE CONTROL: the cap alone cuts the conversation.
#[test]
fn the_memory_row_steps_a_long_resume_line_down_whole() {
    use aterm_agent::harness::resume;
    use aterm_agent::supervise::memory::MemoryOver;
    let session = "0badf00d-1111-2222-3333-444455556666";
    let argv: Vec<String> = [
        "claude",
        "--model",
        "claude-opus-5-5",
        "--add-dir",
        "/Users//someone/Developer/some-project-with-a-long-name",
        "--add-dir",
        "/Users//someone/Developer/another-project-with-a-long-name",
        "--append-system-prompt",
        "keep every answer short and say what you changed",
        "--permission-mode",
        "acceptEdits",
    ]
    .map(String::from)
    .to_vec();
    let line = resume::command(&argv, session).unwrap();
    let over = MemoryOver {
        footprint_mib: 51_302,
        ceiling_mib: 24 * 1024,
        available_mib: Some(12_595),
        strain: None,
        dominant: true,
    };
    let text = over.warning("Claude", true, Some(&line));
    let row = super::supervisor_warning("s-2", "in tab 2", &text);
    assert!(
        row.detail[0].starts_with("Tab 2: Claude uses 50.0 GB"),
        "{:?}",
        row.detail
    );
    let mode = format!(
        "to restart it yourself: {}",
        resume::mode_of(&line).expect("a permission mode")
    );
    assert!(
        mode.contains("acceptEdits") && !mode.contains("--add-dir"),
        "{mode}"
    );
    assert_eq!(row.detail.last(), Some(&mode), "{:?}", row.detail);
    // NEGATIVE CONTROL.
    let cut = atpkg::progress::sanitize_for_tty(&format!("Tab 2: {text}"), 400);
    assert!(!cut.contains(session), "{cut}");

    // Inside the 400 characters, but its clause longer than a detail line:
    // laid out whole, the line would be split across two; it steps down.
    const CLAUSE: &str = "to restart it yourself: ";
    let spare = MemoryOver {
        available_mib: None,
        ..over
    };
    let (text, line) = (1..)
        .map(|n| {
            let dir = format!("/{}", "d".repeat(n));
            let argv = ["claude", "--add-dir", &dir, "--permission-mode", "plan"]
                .map(String::from)
                .to_vec();
            let line = resume::command(&argv, session).unwrap();
            (spare.warning("Claude", true, Some(&line)), line)
        })
        .find(|(_, line)| CLAUSE.len() + line.len() > aterm_messages::DETAIL_LINE_CAP)
        .unwrap();
    let led = format!("Tab 2: {text}");
    assert!(led.chars().count() <= 400, "{led}");
    let row = super::supervisor_warning("s-2", "in tab 2", &text);
    let mode = format!(
        "{CLAUSE}{}",
        resume::mode_of(&line).expect("a permission mode")
    );
    assert!(
        row.detail.last().is_some_and(|l| l.ends_with(&mode)),
        "{:?}",
        row.detail
    );
    // NEGATIVE CONTROL.
    let laid = aterm_messages::text::split_sentence(&led, aterm_messages::DETAIL_LINE_CAP);
    assert!(laid.iter().all(|l| !l.contains(&line)), "{laid:?}");
}

/// A ROW READS BACK AS THE FOLDERS IT NAMES — how a row carried across an
/// update keeps them when the next folder joins it: one folder, several,
/// past the line cap (the named ones and the count), a folder with a space
/// and one longer than a line. The negative control is a row of another
/// shape.
#[test]
fn a_folder_row_reads_back_as_the_folders_it_names() {
    use crate::spawn_folder::{Fault, Named};
    let long = format!("/deep/{}", "d".repeat(300));
    let many: Vec<String> = (0..40).map(|i| format!("/gone/{i}")).collect();
    let cases = [
        named(&["/Users/_an/src/old"]),
        named(&["/Users/_an/My Projects/old"]),
        named(&["/a", "/b", "/c"]),
        named(&[long.as_str()]),
        Named {
            dirs: many[..22].to_vec(),
            unnamed: 18,
        },
    ];
    for case in cases {
        for fault in Fault::ALL {
            assert_eq!(
                folder_row_named(&folder_row(fault, &case)),
                Some(case.clone())
            );
        }
    }
    let past_the_cap = named(&many.iter().map(String::as_str).collect::<Vec<_>>());
    let back = folder_row_named(&folder_row(Fault::Missing, &past_the_cap)).expect("reads");
    assert_eq!((back.dirs.len(), back.unnamed), (22, 18));
    assert_eq!(folder_row_named(&restored_tab_failed("x")), None);
}

/// AUDIT #7 FINDING 48 — the reopened layout's restored line claimed every
/// tab came back in its folder; it COUNTS the panes that could not open
/// theirs instead, and claims folders only when none failed. A pane a
/// program held whose folder is not here (audit #8) is not counted, and
/// the line then claims no folders: its shell started in the default one.
/// The rest of the row is unchanged.
#[test]
fn the_reopened_row_counts_the_panes_that_could_not_open_their_folder() {
    use crate::crash_journal::DeathClass;
    let row = |gone, unasked| {
        journal_reopened_message(
            &reopened_fixture(DeathClass::Killed),
            None,
            None,
            None,
            false,
            PanesWithoutFolder { gone, unasked },
            0,
        )
    };
    for (gone, unasked, claims_folders, counted) in [
        (0, 0, true, None),
        (1, 0, false, Some("1 pane")),
        (2, 0, false, Some("2 panes")),
        (0, 1, false, None),
        (1, 1, false, Some("1 pane")),
    ] {
        let line = &row(gone, unasked).detail[1];
        assert_eq!(
            line.contains("in their folders"),
            claims_folders,
            "{gone}/{unasked}: {line}"
        );
        assert_eq!(
            line.contains("could not open"),
            counted.is_some(),
            "{gone}/{unasked}: {line}"
        );
        if let Some(count) = counted {
            assert!(line.contains(count), "{gone}/{unasked}: {line}");
        }
    }
    let (none, some) = (row(0, 0), row(1, 0));
    assert_eq!(
        (&none.title, none.severity, &none.detail[0], &none.key),
        (&some.title, some.severity, &some.detail[0], &some.key)
    );
}

/// Every R14 row, from the sentences their sites really pass.
fn gesture_failures() -> Vec<Message> {
    vec![
        new_tab_failed("fork: Resource temporarily unavailable (os error 35)"),
        new_window_failed("the pty could not be opened"),
        split_refused(
            "Split refused: this pane is 20x5 cells; a vertical split needs at least 20x7",
        ),
        split_refused("Split refused: this pane's size could not be measured"),
        split_failed("no event loop to host the new pane"),
        keystrokes_dropped(3),
        restore_stopped_early(),
        shell_lost_in_update("descriptor 9 was closed"),
        restored_tab_failed("exec failed"),
        input_refused("the paste is too large \u{2014} paste it in smaller pieces"),
        no_connection(true),
    ]
}

/// THE FULL DISK ACCESS ENTRY ASKS AND GRANTS NOTHING (design-exceptions
/// §8.2, §9.2): one button, which opens the Settings pane, and no `Not
/// now` (the dismissal is the decline). A regression (a verified grant
/// later reads off) is a different key, so a different fact; the keys a
/// grant clears are exactly the two the builder can raise, and none is the
/// retired row's.
#[test]
fn the_file_access_entry_is_honest_and_fence_clean() {
    use crate::consent_card::{NeedCause, NeedWords};
    let words = NeedWords::sample();
    let entry = file_access_entry(&words);
    let msg = entry.message().expect("the words pass the rule");
    assert_eq!(
        msg.actions,
        [Intent::OpenSystemPane {
            pane: PANE_FULL_DISK_ACCESS.to_string()
        }]
    );
    let regressed = file_access_entry(&NeedWords {
        regressed: true,
        ..words
    });
    regressed.message().expect("the words pass the rule");
    let denied = file_access_entry(&NeedWords {
        cause: NeedCause::ProtectedRead { session: None },
        ..words
    });
    denied.message().expect("the words pass the rule");
    assert_eq!(denied.key(), entry.key(), "a refused read is the same need");
    assert_eq!(
        FDA_ENTRY_KEYS,
        [entry.key().as_str(), regressed.key().as_str()],
        "what a grant clears is exactly what the builder can raise"
    );
    assert_ne!(entry.key(), KEY_FILE_ACCESS);
}

/// R14 UNDER THE OWNER'S ATTENTION RULE (2026-09-23): a failure of the
/// person's own gesture is an Error row on a short `HOLD_GESTURE`, keyed to
/// nothing (each is an event of its own), with the WHOLE error behind
/// Details — never cut at 160. A split refusal's excerpt is its sizes,
/// with the sentence whole behind them.
#[test]
fn a_failed_gesture_is_a_short_row_with_the_whole_error_behind_details() {
    let long = "x".repeat(200);
    assert_eq!(
        new_tab_failed(&long).detail,
        std::slice::from_ref(&long),
        "the error whole, not cut at 160"
    );
    for msg in gesture_failures() {
        assert_eq!(msg.severity, Severity::Error, "{msg:?}");
        assert_eq!(msg.hold, Hold::For(HOLD_GESTURE), "{msg:?}");
        assert!(msg.key.is_none(), "an event, not a slot: {msg:?}");
        assert!(!msg.detail.is_empty(), "the reason is behind Details");
    }
    assert_eq!(
        split_refused(
            "Split refused: this pane is 20x5 cells; a vertical split needs at least 20x7"
        )
        .detail,
        [
            "pane 20\u{00d7}5, needs 20\u{00d7}7",
            "this pane is 20x5 cells; a vertical split needs at least 20x7"
        ],
    );
    assert_eq!(
        split_refused("Split refused: this pane's size could not be measured").detail,
        ["this pane's size could not be measured"],
        "another shape is its own excerpt"
    );
}

/// THE REST OF THE RETIRED TOAST UNDER THE ATTENTION RULE: a crippled
/// install is a row only when there is something to fix (no page of
/// aterm's performs the fix, so it carries no action); the grant and the
/// connection disclosure are records that never touch the glass.
#[test]
fn the_rest_of_the_retired_toast_is_rows_only_where_there_is_something_to_fix() {
    use aterm_update::which_copy::InstallPosture;
    assert!(install_posture(InstallPosture::Installed).is_none());
    assert!(install_posture(InstallPosture::NotABundle).is_none());
    for posture in [InstallPosture::MountedImage, InstallPosture::Translocated] {
        let row = install_posture(posture).expect("a fix");
        assert_eq!(row.severity, Severity::Warn, "{posture:?}");
        assert!(row.actions.is_empty(), "{posture:?}");
        assert_eq!(row.key.as_deref(), Some(KEY_INSTALL_POSTURE));
    }
    for record in [
        file_access_granted(),
        session_connection_created(&crate::connections::first_use_notice_text(
            crate::connections::ConnectedSpawnKind::Controlled,
        )),
    ] {
        assert_eq!(record.hold, Hold::LogOnly, "{record:?}");
    }
    assert_eq!(file_access_granted().key.as_deref(), Some(KEY_FILE_ACCESS));
    // A dialog the card is not about: a record per service, naming the tab.
    let dialog = consent_dialog_record("MediaLibrary", "the music and media library", Some(3));
    assert_eq!(dialog.hold, Hold::LogOnly, "{dialog:?}");
    assert_eq!(dialog.tag, tags::PRIVACY);
    assert_eq!(dialog.key.as_deref(), Some("privacy.dialog.medialibrary"));
    assert_eq!(dialog.title, "Asked for the music and media library");
    assert!(dialog.detail[0].contains("tab 3"), "{:?}", dialog.detail);
    let untabbed = consent_dialog_record("Photos", "the Photos library", None);
    assert!(
        !untabbed.detail[0].contains("tab"),
        "an unplaced program names no tab: {:?}",
        untabbed.detail
    );
}

/// The killed row's `Open log` goes to the log, never to the empty crash
/// marker (how aterm knew, not something the reader acts on), and the row
/// takes the crash row's slot.
#[test]
fn the_killed_row_opens_the_log_and_shares_the_crash_slot() {
    let log = "/Users/_an/Library/Logs/aterm/aterm.log";
    let marker = "/Users/_an/Library/Logs/aterm/crash-marker-9-9-app.log.seen";
    let msg = killed_message(&crate::logging::KillEvidence {
        marker: std::path::PathBuf::from(marker),
        log: std::path::PathBuf::from(log),
    });
    assert!(!msg.detail.iter().any(|l| l.contains(marker)));
    assert_eq!(
        msg.actions,
        [Intent::OpenPath {
            path: log.to_string()
        }]
    );
    assert_eq!(msg.key.as_deref(), Some(KEY_CRASH));
}

/// A crash's and a kill's evidence, as the keeper's row receives them.
fn keeper_evidence_fixtures() -> (crate::logging::CrashEvidence, crate::logging::KillEvidence) {
    (
        crate::logging::CrashEvidence {
            path: std::path::PathBuf::from(
                "/Users/_an/Library/Logs/aterm/crash-signal-1-1.log.seen",
            ),
            head: vec!["aterm-gui 0.1.0 crashed".into()],
        },
        crate::logging::KillEvidence {
            marker: std::path::PathBuf::from(
                "/Users/_an/Library/Logs/aterm/crash-marker-9-9-app.log.seen",
            ),
            log: std::path::PathBuf::from("/Users/_an/Library/Logs/aterm/aterm.log"),
        },
    )
}

/// The keeper's recovery row takes the crash row's slot, so it says
/// where that row's evidence is — the crash log, else the killed run's
/// log — while `End sessions` stays its one action; and it never claims
/// a screen was redrawn, only that its program was asked to.
#[test]
fn the_keeper_row_keeps_the_evidence_of_the_row_it_replaces() {
    let (crash, killed) = keeper_evidence_fixtures();
    let blank = crate::crash_restore::RungCounts::default();
    let bare = keeper_recovered_message(2, 1, blank, 0, None, None);
    assert_eq!(bare.key.as_deref(), Some(KEY_CRASH));
    assert_eq!(bare.actions, [Intent::EndRecovered]);
    assert_eq!(
        bare.detail,
        [
            "2 sessions kept running and were reattached",
            KEEPER_SCREENS,
            "1 session could not be reattached and was ended",
        ]
    );
    assert!(!KEEPER_SCREENS.contains("was redrawn"));
    let crashed = keeper_recovered_message(1, 0, blank, 0, Some(&crash), Some(&killed));
    assert_eq!(
        crashed.detail.last().map(String::as_str),
        Some("crash log at /Users/_an/Library/Logs/aterm/crash-signal-1-1.log.seen"),
        "a crash names its log, and wins over a kill"
    );
    assert_eq!(crashed.actions, [Intent::EndRecovered]);
    let stopped = keeper_recovered_message(1, 0, blank, 0, None, Some(&killed));
    assert_eq!(
        stopped.detail.last().map(String::as_str),
        Some("aterm's last lines are in /Users/_an/Library/Logs/aterm/aterm.log"),
        "named: after the screens line, `its` would read as a session's"
    );
}

/// THE ROW SAYS HOW THE SCREENS CAME BACK (crash restore, increment 4) AND THEIR
/// SCROLLBACK (increment 5), held to what the counts select rather than to its wording:
/// every screen exact with every line back reads as the one sentence for it, and no
/// screen from a region keeps the blank-screen words; otherwise the row names the
/// repainted screens, the blank ones and the lines that did not come back, each by its
/// count; no two cases read alike; a recovery with a blank screen never says "each
/// screen"; none claims a screen was redrawn.
#[test]
fn the_keeper_row_says_how_the_screens_came_back() {
    use crate::crash_restore::RungCounts;
    let counts = |exact, screen, meta| RungCounts {
        exact,
        screen,
        meta,
    };
    let line = |c, lost| keeper_recovered_message(3, 0, c, lost, None, None).detail[1].clone();
    assert_eq!(line(counts(3, 0, 0), 0), KEEPER_SCREENS_EXACT);
    assert_eq!(
        line(counts(0, 0, 3), 0),
        KEEPER_SCREENS,
        "no screen from a region"
    );
    let mut seen = std::collections::BTreeSet::new();
    for (c, lost) in [
        (counts(3, 0, 0), 1),
        (counts(2, 1, 0), 0),
        (counts(0, 3, 0), 4096),
        (counts(1, 1, 1), 0),
        (counts(1, 0, 1), 12),
        (counts(2, 0, 1), 7),
    ] {
        let row = line(c, lost);
        assert!(
            seen.insert(row.clone()),
            "{c:?} {lost}: reads as another case: {row}"
        );
        assert_ne!(
            row, KEEPER_SCREENS_EXACT,
            "{c:?} {lost}: a loss is never all exact"
        );
        assert_eq!(
            row.contains(&format!("{} repainted", c.screen)),
            c.screen > 0,
            "{c:?} {lost}: the repainted screens: {row}"
        );
        assert_eq!(
            row.contains(&format!("{} started blank", c.meta)),
            c.meta > 0,
            "{c:?} {lost}: the blank screens: {row}"
        );
        assert!(
            c.meta == 0 || !row.starts_with("each screen"),
            "{c:?} {lost}: a blank screen was not restored: {row}"
        );
        assert_eq!(
            row.contains(&format!("{lost} line")),
            lost > 0,
            "{c:?} {lost}: the lines that did not come back: {row}"
        );
        assert!(!row.contains("was redrawn"), "{c:?} {lost}: {row}");
    }
}

/// A reopened crash journal of `class`: two tabs in one window.
fn reopened_fixture(class: crate::crash_journal::DeathClass) -> crate::crash_journal::Reopened {
    crate::crash_journal::Reopened {
        manifest: crate::crash_journal::fixtures::layout(&[("/a", "zsh"), ("/b", "vim")]),
        class,
        sources: vec![crate::crash_journal::JournalId { pid: 9, nanos: 9 }],
        writers: vec![9],
        handed: Vec::new(),
        programs: None,
        second_chance: false,
    }
}

/// The fixture with the journal saying which leaves ran a program (D16):
/// `(tab, program)` in its one window.
fn reopened_with_programs(
    class: crate::crash_journal::DeathClass,
    programs: &[(u32, &str)],
) -> crate::crash_journal::Reopened {
    crate::crash_journal::Reopened {
        programs: Some(
            programs
                .iter()
                .map(|(tab, program)| crate::crash_journal::LeafProgram {
                    window: 0,
                    tab: *tab,
                    program: (*program).to_string(),
                    agent: false,
                    again: false,
                })
                .collect(),
        ),
        ..reopened_fixture(class)
    }
}

/// [`reopened_with_programs`] with Claude hosted in tab 2 (ruling 293):
/// its leaf carries a relaunch record naming a conversation, and the
/// journal says the agent ran there.
fn reopened_with_agent(
    class: crate::crash_journal::DeathClass,
    programs: &[(u32, &str)],
) -> crate::crash_journal::Reopened {
    let mut reopened = reopened_with_programs(class, programs);
    let claude = crate::restore::AgentRestore {
        word: String::new(),
        pid: 4242,
        start: "Sat Sep 27 01:02:03 2026".into(),
        program: "/opt/claude/bin/claude".into(),
        argv: vec!["/opt/claude/bin/claude".into()],
        session: Some("0b6f3c1e-8a4d-4b61-9d52-7f1e2c3a4b5c".into()),
        cwd: "/b".into(),
        version: None,
        home: None,
        login: None,
        run: None,
        cause: None,
        tab: None,
    };
    reopened
        .manifest
        .fill_agents(&|id| (id == 1).then(|| claude.clone()));
    if let Some(programs) = reopened.programs.as_mut() {
        programs.push(crate::crash_journal::LeafProgram {
            window: 0,
            tab: 1,
            program: "claude".to_string(),
            agent: true,
            again: false,
        });
    }
    reopened
}

/// A RESTORED CODEX THAT DID NOT COME BACK IS SAID AS CODEX (F1-R8-G4): the
/// restored agents' row hard-coded Claude, so a Codex the queue did not
/// relaunch was told to the owner as a Claude, its remedy `claude --resume`
/// — a command that never brings a Codex thread back. Each tab is worded by
/// the agent that ran there, as the held record is (`restored_agent_held`):
/// Codex's `codex resume` and its thread; a row of both kinds names each
/// tab's command on its own line — and a Codex whose typed relaunch did not
/// come up is said as that, not as one that started (F1-R9-G5). CONTROL: a
/// Claude row reads as before.
#[test]
fn a_restored_codex_that_did_not_come_back_is_said_as_codex() {
    use aterm_agent::harness::relaunch::Outcome;
    use aterm_agent::harness::upgrade::Agent;
    let codex = restored_agents_not_resumed(
        &[(
            "in tab 3".to_string(),
            Agent::Codex,
            Outcome::Cannot("internal-panic".into()),
        )],
        None,
    );
    assert_eq!(codex.title, "Couldn't resume Codex in tab 3");
    assert_eq!(
        codex.detail[0],
        "it could not be started there, so its thread did not come back after aterm stopped: \
         type codex resume there to pick it up again"
    );
    // F1-R9-G5: the Codex lane's `no-resume`, and its `wait:resume` while
    // no relaunched TUI is found, mean the relaunched Codex never came up in
    // the tab — never "Codex started in the tab".
    let mut started = Vec::new();
    for step in ["failed:no-resume", "wait:resume"] {
        let row = restored_agents_not_resumed(
            &[(
                "in tab 3".to_string(),
                Agent::Codex,
                Outcome::NotYet(step.into()),
            )],
            None,
        );
        assert_eq!(
            row.detail[0],
            "the relaunched Codex did not come up in the tab, so its thread did not come back \
             after aterm stopped: type codex resume there to pick it up again",
            "{step}"
        );
        started.push(row);
    }
    let started = &started[0];
    let both_started = restored_agents_not_resumed(
        &[
            (
                "in tab 1".to_string(),
                Agent::Claude,
                Outcome::NotYet("failed:no-resume".into()),
            ),
            (
                "in tab 2".to_string(),
                Agent::Codex,
                Outcome::NotYet("failed:no-resume".into()),
            ),
        ],
        None,
    );
    assert_eq!(
        both_started.detail[1..3],
        [
            "in tab 1, Claude started but did not pick its conversation up: type claude \
             --resume there",
            "in tab 2, the relaunched Codex did not come up in the tab: type codex resume there",
        ],
        "{:?}",
        both_started.detail
    );
    let claude = restored_agents_not_resumed(
        &[(
            "in tab 3".to_string(),
            Agent::Claude,
            Outcome::Cannot("internal-panic".into()),
        )],
        None,
    );
    assert_eq!(claude.title, "Couldn't resume Claude in tab 3", "CONTROL");
    assert!(
        claude.detail[0].ends_with("type claude --resume there to pick it up again"),
        "CONTROL: {:?}",
        claude.detail
    );
    let both = restored_agents_not_resumed(
        &[
            (
                "in tab 1".to_string(),
                Agent::Claude,
                Outcome::Cannot("shell-gone".into()),
            ),
            (
                "in tab 2".to_string(),
                Agent::Codex,
                Outcome::Cannot("shell-gone".into()),
            ),
        ],
        None,
    );
    assert_eq!(both.title, "Couldn't resume the agents in 2 tabs");
    assert_eq!(
        both.detail[1..3],
        [
            "in tab 1, its tab's shell had ended: type claude --resume there",
            "in tab 2, its tab's shell had ended: type codex resume there",
        ],
        "{:?}",
        both.detail
    );
    for row in [&codex, started] {
        assert!(
            row.detail.iter().all(|line| !line.contains("claude")),
            "no Claude in a Codex row: {:?}",
            row.detail
        );
    }
    // F1-R8-G4: the reopened row's resumed line says a Codex as Codex, on its
    // thread; both kinds together as the agents.
    assert_eq!(
        journal_resumed_line(&[((0, 1), Agent::Codex)], 1),
        "aterm starts Codex again on its thread in tab 2"
    );
    assert_eq!(
        journal_resumed_line(&[((0, 0), Agent::Claude), ((0, 1), Agent::Codex)], 1),
        "aterm starts the agents again on their conversations in tabs 1 and 2"
    );
}

fn journal_note_fixtures() -> Vec<crate::crash_journal::Note> {
    let id = crate::crash_journal::JournalId { pid: 9, nanos: 9 };
    vec![
        crate::crash_journal::Note::Unreadable {
            id,
            class: crate::crash_journal::DeathClass::Killed,
            path: Some(std::path::PathBuf::from(
                "/Users/_an/Library/Application Support/aterm/journal-9-9.unreadable",
            )),
            error: "its layout is not one this build reads".into(),
        },
        crate::crash_journal::Note::Relapsed {
            id,
            class: crate::crash_journal::DeathClass::Signal,
            windows: 1,
            tabs: 2,
            lost: Some(crate::crash_journal::Lost {
                tabs: 1,
                named: vec!["Python".to_string()],
                unnamed: 0,
            }),
        },
        crate::crash_journal::Note::Relapsed {
            id,
            class: crate::crash_journal::DeathClass::Killed,
            windows: 1,
            tabs: 2,
            lost: Some(crate::crash_journal::Lost::default()),
        },
    ]
}

/// D16 (ruling 282) — A QUIET RELAUNCH: a killed run whose tabs all sat at
/// their prompts lost nothing a person ran, so the reopened layout is a
/// RECORD, never a held warning. The negative controls: a program that ran
/// keeps the warning and names itself, a crash keeps its row whatever ran,
/// a leaf the layout does not hold counts for nothing, and a journal that
/// did not say (an older writer's) keeps the warning.
#[test]
fn a_relaunch_that_lost_nothing_is_a_record_and_a_lost_program_is_named() {
    use crate::crash_journal::DeathClass;
    let log = std::path::Path::new("/Users/_an/Library/Logs/aterm/aterm.log");
    let reopened = |journal: &crate::crash_journal::Reopened| {
        journal_reopened_message(
            journal,
            None,
            None,
            Some(log),
            false,
            PanesWithoutFolder::NONE,
            0,
        )
    };
    let killed = DeathClass::Killed;
    for (what, journal, severity, hold) in [
        (
            "nothing ran",
            reopened_with_programs(killed, &[]),
            Severity::Info,
            Hold::LogOnly,
        ),
        (
            "vim ran",
            reopened_with_programs(killed, &[(1, "vim")]),
            Severity::Warn,
            Hold::For(HOLD_LAUNCH),
        ),
        (
            "a leaf the layout does not hold",
            reopened_with_programs(killed, &[(7, "vim")]),
            Severity::Info,
            Hold::LogOnly,
        ),
        (
            "a crash",
            reopened_with_programs(DeathClass::Panic, &[]),
            Severity::Error,
            Hold::For(HOLD_LAUNCH),
        ),
        (
            "an older journal",
            reopened_fixture(killed),
            Severity::Warn,
            Hold::For(HOLD_LAUNCH),
        ),
    ] {
        let msg = reopened(&journal);
        assert_eq!((msg.severity, msg.hold), (severity, hold), "{what}");
    }
    assert!(
        reopened(&reopened_with_programs(killed, &[(1, "vim")]))
            .title
            .contains("vim"),
        "the lost program is named"
    );
}

/// A RETIRED SPELLING IS NOT AN UNKNOWN KEY (design ruling 42). atpkg
/// still reads `[packages] auto_update = false` as `enabled = false`, so
/// it is its own family: a record that never reaches the glass. Beside a
/// typo, the typo is the one row (the setting the person silently misses),
/// and the retired spellings — `auto_update`, which still applies, and
/// `channel`, a removed feature's — are the record (ruling 213). The
/// sentences are the validator's own (`app_config::collect_key_notices`).
#[test]
fn a_retired_spelling_is_a_record_and_never_counted_as_an_unknown_key() {
    let mut warns = ConfigWarnings::default();
    crate::app_config::collect_key_notices(
        &mut warns,
        "[packages]\nauto_update = false\nseed_install = false\n",
    );
    let msgs = warns.into_messages();
    assert_eq!(msgs.len(), 1, "one family, not the ignored keys: {msgs:?}");
    assert_eq!(
        msgs[0].key.as_deref(),
        Some(ConfigFamily::RetiredKeys.key())
    );
    assert_eq!(msgs[0].hold, Hold::LogOnly, "a record, never on glass");

    let src = "windw_padding = 20\n\
               [packages]\nenabled = true\nauto_update = false\nchannel = \"stable\"\n";
    let mut warns = ConfigWarnings::default();
    crate::app_config::collect_key_notices(&mut warns, src);
    assert_eq!(warns.sentences().count(), 3, "stderr says every one");
    let msgs = warns.into_messages();
    assert_eq!(msgs.len(), 2, "{msgs:?}");
    let (ignored, retired) = (&msgs[0], &msgs[1]);
    assert_eq!(
        ignored.key.as_deref(),
        Some(ConfigFamily::IgnoredKeys.key())
    );
    assert_ne!(ignored.hold, Hold::LogOnly, "the typo is the row");
    assert!(ignored.detail.iter().any(|l| l.contains("windw_padding")));
    assert!(
        ignored
            .detail
            .iter()
            .all(|l| !l.contains("auto_update") && !l.contains("channel")),
        "{:?}",
        ignored.detail
    );
    assert_eq!(
        retired.key.as_deref(),
        Some(ConfigFamily::RetiredKeys.key())
    );
    assert_eq!(retired.hold, Hold::LogOnly);
    // `--validate-config` still lists every key that needs an edit.
    assert_eq!(
        crate::native_config_language::ignored_key_warnings(src).len(),
        3
    );
}

/// A DIAGNOSTIC IS ONE PROBLEM, NOT ONE LINE (upstream e7dc1feee, ported
/// onto R13). A rejected `aterm.toml` reaches the config lane as the REAL
/// parser's error with its caret diagram: one message whose detail is the
/// diagnostic's rows, the caret row whole on a line of its own, no blank
/// row, and no control character in a cell — a control byte that survives
/// the split is spelled, not dropped.
#[test]
fn a_multi_line_diagnostic_is_one_message_with_the_rows_it_actually_has() {
    let error = "x = [bad\n"
        .parse::<aterm_toml::edit::DocumentMut>()
        .expect_err("a malformed document")
        .to_string();
    assert!(error.contains('\n'), "a multi-line diagnostic: {error:?}");
    let msg = config_lane_error(&format!("Config observation was not valid TOML: {error}"));
    assert_eq!(msg.key.as_deref(), Some(KEY_CONFIG_LANE));
    assert!(msg.detail.len() > 1, "{:?}", msg.detail);
    for line in &msg.detail {
        assert!(
            line.chars().all(|ch| !ch.is_control()),
            "a control character took a cell: {line:?}"
        );
        assert!(!line.trim().is_empty(), "a blank row: {:?}", msg.detail);
    }
    let caret = error
        .lines()
        .find(|row| row.contains('^'))
        .expect("the parser's caret row")
        .trim_end();
    let at = msg
        .detail
        .iter()
        .position(|line| line == caret)
        .unwrap_or_else(|| panic!("the caret row {caret:?} is no line: {:?}", msg.detail));
    assert!(at > 0, "the caret must not share a line with the sentence");
    let hostile = config_lane_error("Config observation was not valid TOML: bad\u{1b}[31m\tx");
    assert_eq!(hostile.detail, ["bad\u{00b7}[31m\u{00b7}x"]);
}

/// A PERSON'S SETTINGS ▸ PACKAGES VERB (design ruling 224): every row it
/// can show — the verb's own, and queued at the store lock — is PROGRESS
/// (a busy meter), keyed to the lane's pass so the plan's reads restate it,
/// revealed only after the progress grace (a 1–4 s no-op never flashes a
/// row), with no capsule; the queued row outlives a wait that may last the
/// lock's whole bound. The machine apply, a sub-second edit, has no row.
#[test]
fn a_persons_packages_rows_are_progress_after_the_grace() {
    for verb in [
        PackagesVerb::Check,
        PackagesVerb::Install,
        PackagesVerb::Remove,
    ] {
        let waiting = packages_waiting_row(verb);
        for msg in [&packages_verb_row(verb), &waiting] {
            assert_eq!(attention(msg), Ok(Attention::Progress), "{}", msg.title);
            assert_eq!(msg.reveal_after, Some(aterm_messages::PROGRESS_GRACE));
            assert_eq!(msg.key.as_deref(), Some(crate::toolchain_words::KEY_PASS));
            assert!(msg.meter.as_ref().is_some_and(|m| m.busy), "busy");
            assert!(msg.actions.is_empty(), "work in flight is no decision");
        }
        assert!(matches!(
            waiting.hold,
            Hold::Live { stale_after } if stale_after
                > Duration::from_secs(crate::ATPKG_WAIT_LOCK_SECS)
        ));
    }
    assert_eq!(
        crate::app_native::person_verb(&crate::native_app::PackagesRequest::MachineApply),
        None
    );
    for (failed, changed, echo) in [
        (true, true, aterm_messages::EchoKind::Fault),
        (false, false, aterm_messages::EchoKind::Vanish),
        (false, true, aterm_messages::EchoKind::Complete),
    ] {
        assert_eq!(crate::app_native::person_echo(failed, changed), echo);
    }
}

/// THE OWNER'S RULE OVER EVERY BUILDER (design §10.1, ruling 76; owner rule
/// 2026-09-28): every message a reporter here or in a lane can post is
/// progress with an indicator, a decision, a failure or a record — never a
/// confirmation, an FYI or a long or clause-split glass title — and goes
/// through a fresh center's door without being RECORDED in its place
/// (`demotions() == 0`: a class that could not stand is a defect).
#[test]
fn every_reporter_message_survives_the_door() {
    let now = std::time::Instant::now();
    let stamp = aterm_messages::WallStamp {
        unix_ms: 1_790_000_000_000,
    };
    let mut c = aterm_messages::MessageCenter::new(aterm_messages::MessageLog::empty(), now);
    for msg in every_lane_message() {
        let title = msg.title.clone();
        if let Err(why) = attention(&msg) {
            panic!("{why}: {title:?} ({:?})", msg.hold);
        }
        c.post(msg, stamp, now);
        assert_eq!(
            c.demotions(),
            0,
            "the door recorded {title:?} ({:?}): a class that cannot stand",
            c.last_demoted()
        );
    }
}

/// Every lane's rows beside this module's: the update flow, its records
/// and outcomes, the toolchain lane's rows and records, a person's
/// Settings ▸ Packages verbs and the session waits — the census the
/// marks are held over (ruling 302).
fn every_lane_message() -> Vec<Message> {
    use crate::toolchain_words as tw;
    use crate::update_words as uw;
    use aterm_update::Progress as P;
    let mut all = every_reporter_message();
    all.extend([
        uw::progress(
            &P::Downloading {
                version: "0.95.0".into(),
                bytes_done: 37_000_000,
                bytes_total: 74_000_000,
            },
            None,
            "",
        ),
        uw::progress(
            &P::Verifying {
                version: "0.95.0".into(),
            },
            None,
            "",
        ),
        uw::staged("0.95.0", 7, Some(uw::ApplyPosture::Automatic)),
        uw::staged("0.95.0", 7, Some(uw::ApplyPosture::ManualByConfig)),
        uw::download_failed("0.95.0", "zip sha256 mismatch"),
        uw::download_postponed("on battery"),
        uw::download_withheld("0.95.0", "held: build 7 is quarantined"),
        uw::installing("0.95.0"),
        uw::finishing("0.95.0"),
        uw::landed("0.95.0", 7, 0, None),
        uw::switch_started(Some("0.95.0"), "0.94.0"),
        uw::switch_stopped(Some("0.95.0"), "0.94.0", true, "typing"),
        uw::switch_stopped(Some("0.95.0"), "0.94.0", false, "child died"),
        uw::outcome("Update waiting", "x", Severity::Info),
        uw::outcome("Couldn't finish the update", "x", Severity::Warn),
        uw::needs_install(
            &crate::app_update_screen::update_installed_title(Some("0.95.0")),
            uw::INSTALL_FROM_MENU,
            Severity::Info,
            7,
        ),
        uw::needs_install("Couldn't install aterm v0.95.0", "x", Severity::Warn, 7),
        uw::needs_install(
            "Couldn't install aterm v0.95.0",
            uw::TRIES_AGAIN,
            Severity::Warn,
            7,
        ),
        uw::failed("Couldn't finish the update", "the helper exited 3", false),
        uw::health_warning(
            uw::HealthKind::Download,
            aterm_update::health_failing_title("pipeline"),
            "3 checks",
        ),
        uw::health_warning(
            uw::HealthKind::Stalled,
            uw::CHECKER_STALLED_TITLE,
            "the update check stopped answering 50 min ago while checking for a new \
             version; aterm started a fresh one in its place.",
        ),
        uw::health_recovered("Couldn't download updates", "since Aug 27"),
        uw::scrollback_lost(12_345, 3),
        tw::announced("installing 10 ALab program(s) over the network (about 3 GB on disk)"),
        tw::deferred("the network is metered"),
        tw::installed("claude 2.1.280"),
        tw::ended("3 programs updated", true),
        tw::failed("trust: disk full"),
        tw::install_failed("trust: disk full"),
        tw::not_installed("nothing to install"),
        aterm_messages::waits::paste_row(3, 1, 1_100_000, 4_200_000, Duration::ZERO),
        aterm_messages::waits::rewrap_row(3, 1_200_000, 3_400_000, Duration::ZERO),
    ]);
    all.extend(tw::machine_settings("universal-control disabled"));
    for verb in [
        PackagesVerb::Check,
        PackagesVerb::Install,
        PackagesVerb::Remove,
    ] {
        all.push(packages_verb_row(verb));
        all.push(packages_waiting_row(verb));
    }
    all
}

/// The lost-integration rows the corpus holds: one tab under each
/// remedy, two tabs in two windows under both, and more tabs than the row
/// names a line each.
fn integration_lost_fixtures() -> Vec<Message> {
    use crate::integration_notice::Heal;
    let tab = |place: &str, sid: &str, heal| LostTab {
        place: place.to_string(),
        sid: sid.to_string(),
        heal,
    };
    let claude = Heal::AgentMoved {
        agent: "Claude Code",
    };
    let mut sets = vec![
        vec![tab("in tab 2", "s-0123456789abcdef", claude)],
        vec![tab(
            "in window 2, tab 1",
            "s-0123456789abcdef",
            Heal::NewTab,
        )],
        vec![
            tab("in window 1, tab 2", "s-0123456789abcdef", claude),
            tab("in window 2, tab 10", "s-fedcba9876543210", Heal::NewTab),
        ],
    ];
    sets.push(
        (1..=14)
            .map(|i| tab(&format!("in tab {i}"), &format!("s-{i:016x}"), claude))
            .collect(),
    );
    sets.iter()
        .filter_map(|tabs| integration_lost(tabs))
        .collect()
}

/// DESIGN (a) OF THE 2026-09-29 REVIEW (rulings 270, 300): the row names
/// each tab by its PLACE, never by the raw `s-<hex>` a person never sees;
/// the session ids ride the technical lines alone, where the successor
/// reads them back whole, however many lines they take — and a one-tab row
/// an older parent carried (it said `sessions`) still reads. An agent tab
/// past the first line's naming cap is still listed. No line carries a
/// space run (the `AgentMoved` remedy once held 18 spaces where its line
/// continuation lost the `\`), the rekey record's included.
#[test]
fn the_lost_integration_row_names_tabs_by_place_never_by_session_id() {
    use crate::integration_notice::Heal;
    for msg in integration_lost_fixtures() {
        assert!(
            sentences(&msg).iter().all(|l| !l.contains("  ")),
            "{:?}",
            msg.detail
        );
        assert_eq!(msg.key.as_deref(), Some(KEY_INTEGRATION_LOST));
        assert!(matches!(msg.hold, Hold::Standing(_)), "{:?}", msg.hold);
        assert!(!msg.title.contains("s-"), "{:?}", msg.title);
        let (technical, person): (Vec<&String>, Vec<&String>) = msg
            .detail
            .iter()
            .partition(|l| super::split_integration_sessions(l).is_some());
        assert!(!technical.is_empty(), "{:?}", msg.detail);
        for line in &person {
            assert!(!line.contains("s-0") && !line.contains("s-f"), "{line:?}");
        }
        assert!(!integration_lost_sessions(&msg.detail).is_empty());
    }
    let many = integration_lost_fixtures().pop().expect("the long one");
    let sids = integration_lost_sessions(&many.detail);
    assert_eq!(sids.len(), 14, "{:?}", many.detail);
    assert!(sids.iter().all(|s| s.len() == 18), "{sids:?}");
    assert!(
        many.detail
            .iter()
            .all(|l| l.chars().count() <= aterm_messages::DETAIL_LINE_CAP),
        "{:?}",
        many.detail
    );
    let carried = vec!["it reads `status integration=degraded` \u{2014} sessions s-0a".into()];
    assert_eq!(
        integration_lost_sessions(&carried),
        vec!["s-0a".to_string()]
    );
    let mut late = (1..=13)
        .map(|i| LostTab {
            place: format!("in tab {i}"),
            sid: format!("s-{i:016x}"),
            heal: Heal::NewTab,
        })
        .collect::<Vec<_>>();
    late.push(LostTab {
        place: "in tab 14".into(),
        sid: format!("s-{:016x}", 14),
        heal: Heal::AgentMoved {
            agent: "Claude Code",
        },
    });
    let late = integration_lost(&late).expect("14 tabs");
    assert!(
        late.detail
            .iter()
            .any(|l| l.starts_with("tab 14 ") && l.ends_with("Claude Code")),
        "{:?}",
        late.detail
    );
    let rec = integration_rekey_waits("in tab 2", "s-0123456789abcdef");
    assert_eq!(rec.hold, Hold::LogOnly);
    assert!(
        sentences(&rec).iter().all(|l| !l.contains("  ")),
        "{:?}",
        rec.detail
    );
}

/// UNDER ONE TAB'S KEY, THE SAME STALL IS THE SAME BY ITS WORDS (the
/// owner's report of 2026-09-27: a restart posted two tabs' stalls the
/// owner had already read): wherever that tab now stands (the place on
/// the move line is not compared), wherever a refused press put its lines
/// (ruling 307: the restated row shifted every index, and a stall the
/// owner saw through came back), and an overdue one whatever its age.
/// NEGATIVE CONTROLS: another stall and another target are other stalls;
/// so is a row before ruling 270 (the move alone on its line) whose words
/// differ, while one whose words are the same is the same stall.
#[test]
fn a_stall_is_the_same_by_its_words_wherever_its_tab_now_stands() {
    use aterm_agent::harness::upgrade::Phase;
    use aterm_agent::harness::upgrade_drive::Row;
    const NOW: u64 = 1_790_529_236;
    let refused = |tab: &str| Row {
        tab: tab.into(),
        from: "2.1.280".into(),
        to: "2.1.283".into(),
        phase: Phase::Failed("not-a-shell-job".into()),
        ..Row::default()
    };
    let (a, b) = ("s-c543f4e0edd3439e5791", "s-0df27d3bf4b3b704621e");
    let row_a = agent_upgrade_stalled(&refused(a), NOW, "in tab 2");
    let row_b = agent_upgrade_stalled(&refused(b), NOW, "in tab 5");
    assert_ne!(row_a.title, row_b.title, "the band tells them apart");
    assert!(
        row_a.detail[..=STALL_MOVE_LINE]
            .iter()
            .all(|l| !l.contains(a)),
        "never the sid there: {:?}",
        row_a.detail
    );
    // THE SAME STALL, wherever its tab now stands.
    let moved = agent_upgrade_stalled(&refused(a), NOW, "in its tab");
    assert!(same_stall(&row_a.detail, &moved.detail));
    let windowed = |tab: &str, window: usize| {
        agent_upgrade_stalled(&refused(tab), NOW, &tab_place(Some(window), 1))
    };
    assert!(same_stall(&row_a.detail, &windowed(a, 2).detail));
    assert!(same_stall(&windowed(a, 1).detail, &windowed(a, 12).detail));
    // AND WHEREVER A REFUSED PRESS PUT ITS LINES: the refusal above them.
    let mut restated = vec!["press it again: another step held the upgrade's lock".to_string()];
    restated.extend(row_a.detail.iter().cloned());
    assert!(same_stall(&restated, &row_a.detail), "{restated:?}");
    assert!(same_stall(&row_a.detail, &restated));
    let overdue = |age: u64| {
        agent_upgrade_stalled(
            &Row {
                phase: Phase::Pending,
                behind_since: NOW - age,
                wait: "not-idle:busy".into(),
                ..refused(a)
            },
            NOW,
            "in tab 2",
        )
    };
    assert!(same_stall(
        &overdue(7 * 3_600).detail,
        &overdue(9 * 3_600).detail
    ));
    // NEGATIVE CONTROLS.
    assert!(
        !same_stall(&row_a.detail, &overdue(7 * 3_600).detail),
        "another stall"
    );
    let newer = agent_upgrade_stalled(
        &Row {
            to: "2.1.284".into(),
            ..refused(a)
        },
        NOW,
        "in tab 2",
    );
    assert!(!same_stall(&row_a.detail, &newer.detail), "another target");

    // A ROW BEFORE RULING 270: the move alone on its line.
    let unplaced = |detail: &[String]| -> Vec<String> {
        let mut d = detail.to_vec();
        d[STALL_MOVE_LINE] = d[STALL_MOVE_LINE]
            .split_once(" \u{00b7} ")
            .map(|(_, moved)| moved.to_string())
            .expect("placed");
        d
    };
    let old = unplaced(&row_a.detail);
    assert!(same_stall(&old, &row_a.detail), "{old:?}");
    assert!(same_stall(&row_a.detail, &old));
    assert!(same_stall(
        &unplaced(&overdue(7 * 3_600).detail),
        &overdue(9 * 3_600).detail
    ));
    // NEGATIVE CONTROLS in that layout too, and words that differ.
    assert!(!same_stall(
        &unplaced(&overdue(7 * 3_600).detail),
        &row_a.detail
    ));
    assert!(!same_stall(&unplaced(&newer.detail), &row_a.detail));
    let mut reworded = old.clone();
    reworded[0] = "the move stopped (not-a-shell-job)".to_string();
    assert!(!same_stall(&reworded, &row_a.detail), "{reworded:?}");
    // A move line whose head is no place is compared whole.
    for head in [
        "tab two",
        "in window two, tab 1",
        "in window 2 tab 1",
        "in window 2, tab",
    ] {
        let mut odd = row_a.detail.clone();
        odd[STALL_MOVE_LINE] = format!("{head} \u{00b7} {}", old[STALL_MOVE_LINE]);
        assert!(!same_stall(&odd, &row_a.detail), "{odd:?}");
    }
}

/// ONE TAB'S UPGRADE KEY, AND NOTHING ELSE (review of 2026-09-27): the
/// carried-row sweep (`upgrade_host::post_upgrade_stalls`) withdraws
/// every live row this filter matches that its view does not own, so a
/// match too wide would take other families' rows down. A tab's key —
/// this build's, cleaned as it prints, or an earlier build's — names its
/// tab; the waiting record's bare key, an empty tab, the finished moves'
/// `done`, a key that only begins with the family's word, and every
/// other family's key name none.
#[test]
fn a_tabs_upgrade_key_names_its_tab_and_no_other_key_does() {
    assert_eq!(agent_upgrade_key_tab("harness.upgrade.s-x"), Some("s-x"));
    let key = agent_upgrade_key("s-c543f4e0edd3439e5791");
    assert_eq!(agent_upgrade_key_tab(&key), Some("s-c543f4e0edd3439e5791"));
    for other in [
        KEY_AGENT_UPGRADE,
        "harness.upgrade.",
        "harness.upgrade.done",
        "harness.upgradex.s-x",
        "harness.upgrades.s-x",
        "harness.s-x",
        "harness.relaunch.s-x",
        "fabric.x",
        "",
    ] {
        assert_eq!(agent_upgrade_key_tab(other), None, "{other:?}");
    }
}

// ---- round 44: the second adversarial hunt's host repros -------------

/// Overdue rows of `n` Claude tabs, each with a real-length tab id
/// (`s-` and 20 hex digits).
fn r44_overdue_tabs(
    n: u64,
    now: u64,
    phase: &aterm_agent::harness::upgrade::Phase,
) -> Vec<aterm_agent::harness::upgrade_drive::Row> {
    (0..n)
        .map(|i| aterm_agent::harness::upgrade_drive::Row {
            session: format!("conv-{i}"),
            tab: format!("s-{i:020x}"),
            from: "2.1.283".into(),
            to: "2.1.284".into(),
            source: "managed".into(),
            phase: phase.clone(),
            behind_since: now - 25 * 3_600 - i * 60,
            wait: "queued".into(),
            wait_since: now - 25 * 3_600,
            ..Default::default()
        })
        .collect()
}

/// `UPGRADE NOW` ONLY WHERE IT MOVES (round eight, #4's review and its
/// review). A quiet round its agent's own work gave up on — the work seen
/// since its notice, or running under the agent at the last look — is a
/// wait on that work: `Upgrade now` re-arms a round the quiet restart's
/// gate holds on the same work, so the row offered a button that moved
/// nothing. Its words name what holds it. NEGATIVE CONTROLS keep the
/// button: a quiet give-up no work held (a status that only lagged), the
/// ask flow's give-up, a CODEX round (its lane re-arms a resting round only
/// when the rest is over), and a Claude Code round whose status still says
/// `shell` over ended shells — the owner's `--now` moves all of these.
#[test]
fn a_quiet_round_its_own_work_gave_up_on_offers_no_upgrade_now() {
    use aterm_agent::harness::upgrade::{Agent, GAVE_UP, Phase};
    use aterm_agent::harness::upgrade_drive::{HeldBy, Row};
    use aterm_messages::UpgradeWord;
    const NOW: u64 = 1_790_311_076;
    let tabs = r44_overdue_tabs(2, NOW, &Phase::Pending);
    let gave_up = Row {
        phase: Phase::Failed(GAVE_UP.to_string()),
        retry_at: NOW + 3_600,
        release: GAVE_UP.to_string(),
        wait: "failed".into(),
        quiet: true,
        ..tabs[1].clone()
    };
    let held_by = vec![HeldBy {
        pid: 4242,
        name: "zsh".into(),
        since: NOW - 7_200,
    }];
    let seen = Row {
        work_seen: true,
        ..gave_up.clone()
    };
    let running = Row {
        held_by: held_by.clone(),
        ..gave_up.clone()
    };
    for (name, row, moves) in [
        ("work seen", seen.clone(), false),
        ("work running", running.clone(), false),
        (
            "work seen and running",
            Row {
                held_by,
                ..seen.clone()
            },
            false,
        ),
        ("a status that lagged", gave_up.clone(), true),
        (
            "the ask flow",
            Row {
                quiet: false,
                ..seen.clone()
            },
            true,
        ),
        (
            "codex",
            Row {
                agent: Agent::Codex,
                ..seen.clone()
            },
            true,
        ),
        (
            "a status still saying shell",
            Row {
                wait: "status-stale:shell".into(),
                ..seen.clone()
            },
            true,
        ),
    ] {
        assert_eq!(
            super::agent_upgrade_words(&row, NOW).contains(&UpgradeWord::Now),
            moves,
            "{name}"
        );
        let msg = super::agent_upgrade_stalled(&row, NOW, "in tab 2");
        assert_eq!(
            msg.actions.iter().any(|intent| matches!(
                intent,
                Intent::AgentUpgrade {
                    word: UpgradeWord::Now,
                    ..
                }
            )),
            moves,
            "{name}: {:?}",
            msg.actions
        );
    }
    assert!(
        running
            .held_words(NOW)
            .is_some_and(|words| words.contains("zsh")),
        "what holds it is named"
    );
}

/// THE LOGIN LIFECYCLE'S WORDS ARE SHOWABLE (accounts v2 S7): the due line,
/// the day's warning, the wrong account, each a valid entry whose title
/// counts and whose detail names the labels, never an address; and the
/// machine's renew ask, a standing Now whose one step renews every label,
/// or the whole batch where the labels would not fit.
#[test]
fn the_login_lifecycle_entries_and_the_renew_ask_are_showable() {
    let labels = |names: &[&str]| -> Vec<String> { names.iter().map(|n| n.to_string()).collect() };
    let due = super::accounts_due_entry(&labels(&["m7-a", "pool-1"]))
        .expect("an entry")
        .message()
        .expect("a valid entry");
    assert_eq!(due.title, "2 Claude logins due for renewal");
    assert!(
        due.detail.iter().any(|l| l == "Due: m7-a, pool-1"),
        "{:?}",
        due.detail
    );
    assert!(super::accounts_due_entry(&[]).is_none());
    let none = super::accounts_none_soon_entry()
        .message()
        .expect("a valid entry");
    assert_eq!(none.severity, Severity::Warn);
    let wrong = super::accounts_wrong_entry(&labels(&["m7-a"]))
        .expect("an entry")
        .message()
        .expect("a valid entry");
    assert!(
        wrong
            .detail
            .iter()
            .any(|l| l == "Run aterm harness accounts renew m7-a"),
        "{:?}",
        wrong.detail
    );
    for text in due.detail.iter().chain(&none.detail).chain(&wrong.detail) {
        assert!(!text.contains('@'), "{text}");
    }
    let ask = super::renew_ask(&labels(&["m7-a"]), &["in tab 3".to_string()]);
    assert!(matches!(ask.hold, aterm_messages::Hold::Standing(_)));
    assert_eq!(ask.key.as_deref(), Some(super::RENEW_ASK_KEY));
    assert!(ask.title.ends_with("in tab 3"), "{}", ask.title);
    let many: Vec<String> = (0..20).map(|i| format!("account-number-{i:02}")).collect();
    let ask = super::renew_ask(&many, &["in tab 3".to_string(), "in tab 4".to_string()]);
    assert!(
        ask.detail
            .iter()
            .any(|l| l == "Run aterm harness accounts renew --due"),
        "the batch where the labels do not fit: {:?}",
        ask.detail
    );
    assert_eq!(ask.title, "Claude's login ended in 2 tabs");
}
