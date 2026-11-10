// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! A click on the permission chip ([`crate::harness::permission_click`]), driven for real over a
//! stand-in agent, shell and tab: the gate it waits at, the line it types, and the records it is
//! refused by or stands over.

use super::*;
use crate::harness::permission_click::{Click, Look, Refusal, Wait, lane_opts, look_with};
use crate::harness::relaunch::{CAUSE_MEMORY, CAUSE_PERMISSION};
use crate::supervise::policy::turn_end::PermissionTo;

/// The click's Unix second.
const AT: i64 = 1_762_000_500;

/// The signal reply that refuses every signal (a person's keys at the tab): a click that went
/// says `signal-held`, its claim restored, and nothing ends.
const SIGNAL_HELD: &str = "ERR busy person human_ms=300";

fn click(to: PermissionTo) -> Click {
    Click {
        tab: TAB.to_string(),
        conversation: SESSION.to_string(),
        to,
        at: AT,
    }
}

/// What the stand-in tab holds for one look.
#[derive(Default)]
struct Tab {
    /// The tab's control answers (`meta` set to a session with no trusted folder unless set).
    answers: Answers,
    /// The agent's registry status (`None`: `idle`).
    agent_status: Option<&'static str>,
    /// The tab's `status` reply (`None`: the instance's own, no hold and no hand).
    tab_status: Option<&'static str>,
    /// The processes the kernel names under the agent.
    background: Vec<String>,
    /// The conversation's record before the look, of the agent and its shell.
    prior: Option<Box<dyn FnOnce(u32, u32) -> St>>,
    /// The stand-in shell's environment changed after [`OUTSIDE_ROUTE`] ([`stand_in_shell_with`]).
    shell_env: &'static [(&'static str, Option<&'static str>)],
}

/// What [`in_tab`] saw.
struct Seen<T> {
    /// What the act answered.
    said: T,
    /// The conversation's record after it.
    saved: Option<St>,
    /// The record's bytes before the act and after it.
    bytes: (Option<Vec<u8>>, Option<Vec<u8>>),
    /// The requests the tab's stand-in was asked.
    asked: Vec<String>,
}

impl<T> Seen<T> {
    fn signalled(&self) -> bool {
        self.asked.iter().any(|l| l.contains(" signal "))
    }

    fn handled(&self) -> bool {
        self.asked.iter().any(|l| l.contains(" lease "))
    }
}

/// `act` run once over a stand-in Claude Code (running `--permission-mode plan`, its conversation
/// [`SESSION`] resumable) in [`TAB`] as `tab` sets it up, with the click lane's options.
fn in_tab<T>(name: &str, tab: Tab, act: impl FnOnce(&Opts, Snapshot, &dyn Kernel) -> T) -> Seen<T> {
    let dir = scratch(name);
    let answers = Answers {
        meta: if tab.answers.meta.starts_with("ERR unscripted") {
            "OK cwd=/ trusted_cwd=-".to_string()
        } else {
            tab.answers.meta.clone()
        },
        ..tab.answers
    };
    let (sock, asked) = instance_with(&dir, answers);
    let opts = lane_opts(
        &Opts {
            sock: Some(sock),
            ..drive(&dir)
        },
        &click(PermissionTo::Settings),
    );
    seed_builds(&opts);
    let shell = stand_in_shell_with(None, &[&[OUTSIDE_ROUTE][..], tab.shell_env].concat());
    let mut agent = parked_bare().spawn().expect("agent");
    wait_exec(agent.id());
    register(&opts.home, agent.id(), SESSION);
    if let Some(status) = tab.agent_status {
        let file = opts
            .home
            .join(format!(".claude/sessions/{}.json", agent.id()));
        let text = std::fs::read_to_string(&file).expect("the session file");
        let text = text.replace(r#""status":"idle""#, &format!(r#""status":"{status}""#));
        std::fs::write(&file, text).expect("the session file's status");
    }
    if let Some(status) = tab.tab_status {
        std::fs::write(dir.join("t.status"), status).expect("the tab's status");
    }
    std::fs::create_dir_all(state_dir(&opts)).expect("state");
    if let Some(prior) = tab.prior {
        save(&opts, SESSION, &prior(agent.id(), shell.id()));
    }
    write_transcript(
        &opts.home,
        &[answer_at("claude-opus-5-5", "2099-01-01T00:00:00Z")],
    );
    let snap = Snapshot {
        launch_env: None,
        auth_observed: true,
        route: crate::harness::accounts::AuthRoute::Outside,
        profile: Some(live_profile(&opts, None)),
        tab: TAB.to_string(),
        pid: agent.id(),
        start: kernel_start(agent.id()).expect("lstart"),
        shell: shell.id(),
        program: PathBuf::from("/opt/claude/bin/claude"),
        argv: ["/opt/claude/bin/claude", "--permission-mode", "plan"]
            .iter()
            .map(|w| (*w).to_string())
            .collect(),
        session: Some(SESSION.to_string()),
        cwd: "/".to_string(),
        version: Some("1.0.0".to_string()),
        run: crate::harness::relaunch::AgentRun::Claude,
        dialect: None,
        login: None,
        home: None,
        claude_dir: None,
    };
    let script = Script::new(shell.id(), usize::MAX, None);
    let kernel = ParkedProcess {
        background: tab.background,
        ..ParkedProcess::new(&script, snap.pid, Some(&snap.start))
    };
    let record = state_path(&opts, SESSION);
    let before = std::fs::read(&record).ok();
    let said = act(&opts, snap, &kernel);
    let after = std::fs::read(&record).ok();
    let saved = load(&opts, SESSION);
    let _ = agent.kill();
    let _ = agent.wait();
    drop(shell);
    let asked = asked.lock().map(|a| a.clone()).unwrap_or_default();
    Seen {
        said,
        saved,
        bytes: (before, after),
        asked,
    }
}

/// One look at a click toward `to` in `tab` ([`in_tab`]).
fn clicked(name: &str, to: PermissionTo, tab: Tab) -> Seen<Look> {
    in_tab(name, tab, |opts, snap, k| {
        look_with(opts, &click(to), false, &|| true, k, || Ok(snap))
    })
}

/// A screen of `rows` as the tab's `text --json` answers it, the cursor on its last empty caret
/// row.
fn screen_of(rows: &[String]) -> String {
    let caret = rows
        .iter()
        .rposition(|r| r.trim_end() == "❯")
        .unwrap_or(rows.len().saturating_sub(1));
    aterm_json::to_string(&aterm_json::json!({
        "rows": rows.to_vec(),
        "cursor": {"row": caret, "col": 2},
        "seq": 77,
        "human_ms": null,
        "first": 0
    }))
    .expect("a screen")
}

/// Claude Code 2.1.268 midway through a slash press (`claude-2.1.268-slash-menu.txt`, MEASURED):
/// `/cl` in its composer, the slash menu open under the frame, the cursor after the text.
fn slash_press_screen() -> String {
    let rows: Vec<String> =
        include_str!("../../../aterm-phase/src/fixtures/claude-2.1.268-slash-menu.txt")
            .lines()
            .skip(1)
            .map(str::to_owned)
            .collect();
    let caret = rows
        .iter()
        .position(|r| r.starts_with('❯') && r.ends_with("/cl"))
        .expect("the press's text in the composer");
    aterm_json::to_string(&aterm_json::json!({
        "rows": rows,
        "cursor": {"row": caret, "col": 5},
        "seq": 77,
        "human_ms": null,
        "first": 0
    }))
    .expect("a screen")
}

/// A CLICK GOES AS A PERSON'S PRESS OF THE ⬆️ GOES (design §3, "When it goes"): it waits for a
/// turn, a box, a draft (a slash press's text among them), a held tab, a usage limit or a login,
/// each its own wait, with no hand taken and nothing signalled; and it goes past the agent's
/// background shells (its status `shell`, the shells under it) and past the work its screen
/// names, which hold every restart in place alike. The signal is refused here, so a click that went says `signal-held` and leaves no
/// record. NEGATIVE CONTROL: in the same tab a lost line's restart waits (`wait:changed`),
/// nothing signalled.
#[test]
fn a_click_waits_on_what_holds_a_press_and_goes_past_the_agents_own_work() {
    use aterm_phase::prompt::fixtures as f;
    const WEEKLY: &str =
        "  ⎿  You've hit your weekly limit · resets Sep 19 at 11am (America/Los_Angeles)";
    let mut wrong = Vec::new();
    let on = |screen: String| Tab {
        answers: Answers {
            screen,
            signal: SIGNAL_HELD,
            ..Answers::default()
        },
        ..Tab::default()
    };
    let holds = [
        ("turn", on(mid_turn_screen()), Wait::Turn),
        ("box", on(screen_of(&f::screen(f::BOX_RM))), Wait::Box),
        ("draft", on(composer_screen("half a thought")), Wait::Draft),
        ("slash-press", on(slash_press_screen()), Wait::Draft),
        (
            "held",
            Tab {
                tab_status: Some("OK schema=1 hold=1 hand=-"),
                ..on(idle_screen())
            },
            Wait::Held,
        ),
        ("limit", on(idle_under(&[WEEKLY, ""])), Wait::Limit),
        (
            "login",
            on(screen_of(&f::screen(f::LOGIN_EXPIRED))),
            Wait::Login,
        ),
    ];
    for (tag, tab, wait) in holds {
        let seen = clicked(&format!("click-holds-{tag}"), PermissionTo::Bypass, tab);
        if seen.said != Look::Waits(wait.clone())
            || seen.signalled()
            || seen.handled()
            || seen.saved.is_some()
        {
            wrong.push(format!(
                "{tag}: {:?} signalled={} handled={} saved={}",
                seen.said,
                seen.signalled(),
                seen.handled(),
                seen.saved.is_some()
            ));
        }
    }
    let own_work = || {
        [
            (
                "shells",
                Tab {
                    agent_status: Some("shell"),
                    background: vec!["zsh".to_string()],
                    ..on(idle_screen())
                },
            ),
            (
                "screen-work",
                on(idle_under(&[
                    "✻ Worked for 5s · done 4:51 PM · 1 local agent still running",
                    "",
                ])),
            ),
        ]
    };
    for (tag, tab) in own_work() {
        let seen = clicked(&format!("click-goes-{tag}"), PermissionTo::Bypass, tab);
        if seen.said != Look::Waits(Wait::Other("signal-held".to_string()))
            || !seen.signalled()
            || seen.saved.is_some()
        {
            wrong.push(format!(
                "{tag}: {:?} signalled={} saved={}",
                seen.said,
                seen.signalled(),
                seen.saved.is_some()
            ));
        }
    }
    for (tag, tab) in own_work() {
        let lost = Restart::InputLost {
            of: (None, None),
            line: LINE_A,
        };
        let seen = in_tab(&format!("lost-line-holds-{tag}"), tab, |opts, snap, k| {
            crate::harness::relaunch::restart_as(
                opts,
                &lost,
                &ask_of(&lost),
                &crate::harness::accounts::Roster::default(),
                Ok(snap),
                None,
                k,
            )
            .report
            .step
        });
        if seen.said != "wait:changed" || seen.signalled() {
            wrong.push(format!(
                "control {tag}: {:?} signalled={}",
                seen.said,
                seen.signalled()
            ));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// A RECORD UNDER NATIVE COORDINATION REFUSES A CLICK BEFORE ANYTHING IS TAKEN OR WRITTEN
/// (decision 4, `ATERM-KI-1106-3`): with no constructor here to restart it (no harness token),
/// the click is refused `native-coordinated` with no hand taken, nothing signalled and the
/// record's bytes as they were. NEGATIVE CONTROL: the same record uncoordinated lets the click on
/// to its signal.
#[test]
fn a_coordinated_record_refuses_a_click_before_the_hand_and_any_write() {
    for coordinated in [true, false] {
        let seen = clicked(
            &format!("click-coordinated-{coordinated}"),
            PermissionTo::Settings,
            Tab {
                answers: Answers {
                    signal: SIGNAL_HELD,
                    ..Answers::default()
                },
                prior: Some(Box::new(move |_, shell| St {
                    phase: Phase::Done,
                    cause: CAUSE_MEMORY.to_string(),
                    tab: TAB.to_string(),
                    pid: dead_pid(),
                    shell,
                    from: "1.0.0".to_string(),
                    to: "1.0.0".to_string(),
                    done_at: now_s() - 60,
                    native_coordination: coordinated,
                    ..St::default()
                })),
                ..Tab::default()
            },
        );
        if coordinated {
            assert_eq!(seen.said, Look::Refused(Refusal::Coordinated));
            assert!(!seen.handled() && !seen.signalled(), "{:#?}", seen.asked);
            assert!(seen.bytes.0.is_some(), "the record stood before the click");
            assert_eq!(seen.bytes.0, seen.bytes.1, "the record is as it was");
        } else {
            assert_eq!(
                seen.said,
                Look::Waits(Wait::Other("signal-held".to_string()))
            );
            assert!(seen.signalled(), "control: {:#?}", seen.asked);
        }
    }
}

/// ANOTHER RESTART IN FLIGHT KEEPS A CLICK STANDING (design Appendix, "In flight"): over the
/// conversation's record of a memory restart, or of another click (an earlier ask), mid-relaunch,
/// the click waits for it, nothing signalled and nothing taken, and leaves that record as it was
/// for its own lane to carry.
#[test]
fn another_restart_in_flight_keeps_a_click_standing() {
    for (tag, cause, ask) in [
        ("memory", CAUSE_MEMORY, String::new()),
        (
            "earlier-click",
            CAUSE_PERMISSION,
            format!("permission@{}", AT - 60),
        ),
    ] {
        let seen = clicked(
            &format!("click-over-{tag}"),
            PermissionTo::Bypass,
            Tab {
                prior: Some(Box::new(move |_, shell| St {
                    phase: Phase::Relaunched { at_s: now_s() },
                    cause: cause.to_string(),
                    ask,
                    tab: TAB.to_string(),
                    pid: dead_pid(),
                    shell,
                    exited_at: now_s() - 3,
                    ..St::default()
                })),
                ..Tab::default()
            },
        );
        assert_eq!(seen.said, Look::Waits(Wait::Restart), "{tag}");
        assert!(
            !seen.signalled() && !seen.handled(),
            "{tag}: {:#?}",
            seen.asked
        );
        assert_eq!(seen.bytes.0, seen.bytes.1, "{tag}: the record is as it was");
    }
}

/// A RECORD RETIRED UNISSUED REFUSES A CLICK (design Appendix, "Coordinated"): in flight for good
/// by `St::in_flight`, every restart over it waits `native-retired-unissued`, so the click is
/// refused `native-coordinated`, nothing taken, signalled or written, rather than standing behind
/// "the restart under way" that nothing carries. Its control is
/// [`another_restart_in_flight_keeps_a_click_standing`]: the same record, not retired, keeps it.
#[test]
fn a_record_retired_unissued_refuses_a_click() {
    let seen = clicked(
        "click-over-retired-unissued",
        PermissionTo::Bypass,
        Tab {
            prior: Some(Box::new(move |_, shell| St {
                phase: Phase::Relaunched { at_s: now_s() },
                cause: CAUSE_MEMORY.to_string(),
                tab: TAB.to_string(),
                pid: dead_pid(),
                shell,
                exited_at: now_s() - 3,
                native_retired_unissued: true,
                ..St::default()
            })),
            ..Tab::default()
        },
    );
    assert!(
        seen.saved
            .as_ref()
            .is_some_and(|st| st.native_retired_unissued),
        "precondition: the record reads retired unissued"
    );
    assert_eq!(seen.said, Look::Refused(Refusal::Coordinated));
    assert!(!seen.signalled() && !seen.handled(), "{:#?}", seen.asked);
    assert_eq!(seen.bytes.0, seen.bytes.1, "the record is as it was");
}

/// A CLICK'S CLAIM NAMES ITS OWN CAUSE AND ASK, AND ITS LINE ITS OWN FLAG (design §3): the record names the click's own cause and ask, and
/// its line is the launch's rid of every permission flag (here `--permission-mode plan`) and given
/// the row's own: none toward the settings' mode, `--dangerously-skip-permissions` once toward bypass. The
/// agent is signalled for real and its relaunch is in flight after the look.
#[test]
fn a_clicks_claim_names_its_cause_and_ask_and_its_line_its_own_flag() {
    for (to, flag) in [
        (PermissionTo::Settings, None),
        (PermissionTo::Bypass, Some("--dangerously-skip-permissions")),
    ] {
        let seen = clicked(&format!("click-line-{}", to.word()), to, Tab::default());
        assert!(
            matches!(seen.said, Look::InFlight(_)),
            "{to:?}: {:?}",
            seen.said
        );
        assert!(seen.signalled(), "{to:?}: {:#?}", seen.asked);
        let st = seen
            .saved
            .unwrap_or_else(|| panic!("{to:?}: the click's claim"));
        assert_eq!(st.cause, CAUSE_PERMISSION, "{to:?}: a resumed click");
        assert_eq!(st.ask, format!("permission@{AT}"), "{to:?}");
        assert!(
            !st.line.contains("--permission-mode"),
            "{to:?}: {}",
            st.line
        );
        assert_eq!(
            st.line.matches("--dangerously-skip-permissions").count(),
            usize::from(flag.is_some()),
            "{to:?}: {}",
            st.line
        );
        assert!(
            st.line.contains("--resume") && st.line.contains(SESSION),
            "{to:?}: resumed: {}",
            st.line
        );
    }
}

/// A CLICK ENDS NO AGENT ITS RELAUNCH COULD NOT ADMIT (the raw relaunch's account check, made
/// before the signal): an external route with no recorded launch environment is read again at
/// the shell when the line is typed, and a shell whose environment names no `HOME` admits none
/// there. The click waits `account` with nothing signalled and the conversation's record as it
/// was. NEGATIVE CONTROL: the same click over a shell with its `HOME` signals its agent.
#[test]
fn a_click_ends_no_agent_its_relaunch_could_not_admit() {
    for env in [&[("HOME", None)][..], &[]] {
        let seen = clicked(
            &format!("click-account-{}", env.len()),
            PermissionTo::Bypass,
            Tab {
                shell_env: env,
                ..Tab::default()
            },
        );
        if env.is_empty() {
            assert!(
                matches!(seen.said, Look::InFlight(_)),
                "control: {:?}",
                seen.said
            );
            assert!(seen.signalled(), "control: {:#?}", seen.asked);
        } else {
            assert_eq!(seen.said, Look::Waits(Wait::Other("account".to_string())));
            assert!(!seen.signalled(), "{:#?}", seen.asked);
            assert_eq!(seen.bytes.0, seen.bytes.1, "the record is as it was");
        }
    }
}

/// A tab another supervisor claims, or a credit-claim worker's, refuses a click before its gate,
/// with nothing taken. The tab of [`a_click_waits_on_what_holds_a_press_and_goes_past_the_agents_own_work`]
/// is their control, unclaimed.
#[test]
fn a_claimed_tab_and_a_claim_workers_tab_refuse_a_click() {
    let claim_folder = format!(
        "OK cwd=/ trusted_cwd=/state/{}/{}",
        crate::harness::cli::HARNESS,
        crate::harness::credit_offers::CLAIM_FOLDER
    );
    for (tag, answers, refusal) in [
        (
            "claimed",
            Answers {
                row: "supervisor=codex-alab",
                ..Answers::default()
            },
            Refusal::Claimed,
        ),
        (
            "claim-worker",
            Answers {
                meta: claim_folder.clone(),
                ..Answers::default()
            },
            Refusal::ClaimWorker,
        ),
    ] {
        let seen = clicked(
            &format!("click-{tag}"),
            PermissionTo::Bypass,
            Tab {
                answers,
                ..Tab::default()
            },
        );
        assert_eq!(seen.said, Look::Refused(refusal), "{tag}");
        assert!(
            !seen.signalled() && !seen.handled() && seen.saved.is_none(),
            "{tag}: {:#?}",
            seen.asked
        );
    }
}
